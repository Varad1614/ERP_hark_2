//! Razorpay REST client — **Part 2 · Stage 4 (S4-pay)**.
//!
//! Plain HTTP over `reqwest` (rustls) against the Razorpay v1 API, speaking only JSON and integer
//! paise (this crate may only depend on `sutra-kernel`); the composition root wraps it as the
//! `sutra_contracts::finance::ports::PaymentGatewayPort` (`apps/api/src/payment_gateway.rs`).
//!
//! | Call | Endpoint | Doc |
//! |------|----------|-----|
//! | create order | `POST /v1/orders` (`receipt` ≤ 40 chars, unique → idempotency key) | <https://razorpay.com/docs/api/orders/create/> |
//! | find order by receipt | `GET /v1/orders?receipt=` | <https://razorpay.com/docs/api/orders/fetch-all/> |
//! | payments of an order | `GET /v1/orders/{id}/payments` | <https://razorpay.com/docs/api/orders/fetch-payments/> |
//! | refund | `POST /v1/payments/{id}/refund` + `X-Refund-Idempotency` | <https://razorpay.com/docs/api/refunds/create-normal/>, <https://razorpay.com/docs/api/refunds/normal-refunds-idempotent/> |
//! | refunds of a payment | `GET /v1/payments/{id}/refunds` | <https://razorpay.com/docs/api/refunds/fetch-multiple-refund-payment/> |
//! | webhook signature | hex HMAC-SHA256(raw body, webhook secret) in `X-Razorpay-Signature` | <https://razorpay.com/docs/webhooks/validate-test/> |
//! | checkout signature | hex HMAC-SHA256(`order_id + "|" + payment_id`, key secret) | <https://razorpay.com/docs/payments/server-integration/nodejs/integration-steps/> (step 5) |
//!
//! * Authentication: HTTP basic `key_id:key_secret` on every call; the secrets never appear in
//!   `Debug`, logs or error messages.
//! * Timeouts: one per-request timeout (`timeout`), connect timeout ≤ 5 s.
//! * Retries (bounded, `max_retries` extra attempts, exponential back-off from `retry_backoff`,
//!   capped at 5 s): transport errors, timeouts, `429` and `5xx`. Order creation is retried only
//!   after asking Razorpay for an order with the same `receipt` (the receipt is derived from our
//!   idempotency key, so a response lost in flight never creates a second order); a refund is
//!   retried with the same `X-Refund-Idempotency` key.
//! * Signatures are compared in constant time (`hmac::Mac::verify_slice`).
//! * Webhook events are deduplicated on `"<event>:<entity id>"` taken from the **signed** body
//!   (the `x-razorpay-event-id` header is not covered by the signature, so a replay could change
//!   it at will).

use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;
use sutra_kernel::error::KernelError;

/// Production API host.
pub const LIVE_BASE_URL: &str = "https://api.razorpay.com";

/// Maximum `receipt` length accepted by the Orders API.
pub const MAX_RECEIPT_LEN: usize = 40;

/// Client settings (`Config::hardening.payments.razorpay`).
#[derive(Clone)]
pub struct RazorpaySettings {
    /// `https://api.razorpay.com` (a local stub in tests).
    pub base_url: String,
    /// `rzp_test_…` / `rzp_live_…` (public).
    pub key_id: String,
    /// API key secret (basic-auth password, checkout signature key).
    pub key_secret: String,
    /// Webhook secret (dashboard → webhooks).
    pub webhook_secret: String,
    /// Per-request timeout.
    pub timeout: Duration,
    /// Extra attempts after the first one (transport errors, 429, 5xx).
    pub max_retries: u32,
    /// First back-off delay (doubles per attempt, capped at 5 s).
    pub retry_backoff: Duration,
}

impl std::fmt::Debug for RazorpaySettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RazorpaySettings")
            .field("base_url", &self.base_url)
            .field("key_id", &self.key_id)
            .field("key_secret", &"<redacted>")
            .field("webhook_secret", &"<redacted>")
            .field("timeout", &self.timeout)
            .field("max_retries", &self.max_retries)
            .field("retry_backoff", &self.retry_backoff)
            .finish()
    }
}

/// A Razorpay order (`entity = order`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RzOrder {
    pub id: String,
    /// Paise.
    pub amount: i64,
    pub currency: String,
    pub receipt: Option<String>,
    /// `created | attempted | paid`.
    pub status: String,
}

/// A Razorpay payment (`entity = payment`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RzPayment {
    pub id: String,
    pub order_id: Option<String>,
    /// Paise.
    pub amount: i64,
    pub currency: String,
    /// `created | authorized | captured | refunded | failed`.
    pub status: String,
    pub captured: bool,
    pub error_code: Option<String>,
    pub error_description: Option<String>,
}

/// A Razorpay refund (`entity = refund`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RzRefund {
    pub id: String,
    pub payment_id: String,
    /// Paise.
    pub amount: i64,
    /// `pending | processed | failed`.
    pub status: String,
    pub receipt: Option<String>,
}

/// What the payments of an order say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RzOutcome {
    /// A captured payment (money received).
    Captured(RzPayment),
    /// Every attempt failed (the last one is returned).
    Failed(RzPayment),
    /// No attempt yet, or an attempt in flight (`created` / `authorized`).
    Pending,
}

/// A verified webhook.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RzWebhook {
    /// `"<event>:<entity id>"` from the signed body (idempotency key).
    pub event_id: String,
    pub event: String,
    /// Order of the payment (empty for events without one, e.g. `refund.*`).
    pub order_id: String,
    pub payment: Option<RzPayment>,
}

// ------------------------------------------------------------------ pure helpers

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
        .collect()
}

/// Lowercase hex HMAC-SHA256 of `message` keyed with `key`.
pub fn hmac_hex(key: &[u8], message: &[u8]) -> String {
    match Hmac::<Sha256>::new_from_slice(key) {
        Ok(mut mac) => {
            mac.update(message);
            hex(&mac.finalize().into_bytes())
        }
        Err(_) => String::new(),
    }
}

/// Constant-time check of a lowercase / uppercase hex HMAC-SHA256 signature.
pub fn verify_hmac_hex(key: &[u8], message: &[u8], signature: &str) -> bool {
    let sig = signature.trim();
    if key.is_empty() || sig.len() != 64 {
        return false;
    }
    let Some(bytes) = unhex(sig) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(key) else {
        return false;
    };
    mac.update(message);
    mac.verify_slice(&bytes).is_ok()
}

/// `X-Razorpay-Signature` of a webhook body.
pub fn webhook_signature(webhook_secret: &str, body: &[u8]) -> String {
    hmac_hex(webhook_secret.as_bytes(), body)
}

/// `razorpay_signature` of a checkout callback.
pub fn checkout_signature(key_secret: &str, order_id: &str, payment_id: &str) -> String {
    hmac_hex(
        key_secret.as_bytes(),
        format!("{order_id}|{payment_id}").as_bytes(),
    )
}

fn sha_hex(input: &str) -> String {
    hex(&Sha256::digest(input.as_bytes()))
}

/// Order `receipt` derived from our idempotency key (`sutra_` + 32 hex = 38 chars ≤ 40).
pub fn receipt_for(idempotency_key: &str) -> String {
    format!("sutra_{}", &sha_hex(idempotency_key)[..32])
}

/// Refund `receipt` + `X-Refund-Idempotency` value derived from our idempotency key
/// (`sutra-rf-` + 31 hex = 40 chars; letters, digits and hyphens only).
pub fn refund_key_for(idempotency_key: &str) -> String {
    format!("sutra-rf-{}", &sha_hex(idempotency_key)[..31])
}

/// `true` for a Razorpay id such as `order_DESlLckIVRkHWj` (safe to put in a URL path).
pub fn valid_id(prefix: &str, id: &str) -> bool {
    id.len() <= 64
        && id
            .strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric()))
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|x| !x.is_empty())
        .map(str::to_owned)
}

fn i(v: &Value, k: &str) -> Option<i64> {
    v.get(k).and_then(Value::as_i64)
}

/// Parses a payment entity.
pub fn parse_payment(v: &Value) -> Option<RzPayment> {
    Some(RzPayment {
        id: s(v, "id")?,
        order_id: s(v, "order_id"),
        amount: i(v, "amount")?,
        currency: s(v, "currency").unwrap_or_else(|| "INR".into()),
        status: s(v, "status")?,
        captured: v.get("captured").and_then(Value::as_bool).unwrap_or(false),
        error_code: s(v, "error_code"),
        error_description: s(v, "error_description"),
    })
}

/// Parses an order entity.
pub fn parse_order(v: &Value) -> Option<RzOrder> {
    Some(RzOrder {
        id: s(v, "id")?,
        amount: i(v, "amount")?,
        currency: s(v, "currency").unwrap_or_else(|| "INR".into()),
        receipt: s(v, "receipt"),
        status: s(v, "status").unwrap_or_else(|| "created".into()),
    })
}

/// Parses a refund entity.
pub fn parse_refund(v: &Value) -> Option<RzRefund> {
    Some(RzRefund {
        id: s(v, "id")?,
        payment_id: s(v, "payment_id").unwrap_or_default(),
        amount: i(v, "amount")?,
        status: s(v, "status").unwrap_or_else(|| "pending".into()),
        receipt: s(v, "receipt"),
    })
}

fn items(v: &Value) -> Vec<Value> {
    v.get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The outcome of an order from its payments: a captured one wins (also when it was refunded
/// later: the money was received); an attempt in flight keeps it pending; only failures → failed.
pub fn outcome_of(payments: &[RzPayment]) -> RzOutcome {
    if let Some(p) = payments
        .iter()
        .find(|p| p.captured || p.status == "captured")
    {
        return RzOutcome::Captured(p.clone());
    }
    if payments.is_empty()
        || payments
            .iter()
            .any(|p| matches!(p.status.as_str(), "created" | "authorized"))
    {
        return RzOutcome::Pending;
    }
    match payments.iter().rev().find(|p| p.status == "failed") {
        Some(p) => RzOutcome::Failed(p.clone()),
        None => RzOutcome::Pending,
    }
}

/// Parses a (verified) webhook body. Unknown events parse with `payment = None`.
pub fn parse_webhook(body: &[u8]) -> Result<RzWebhook, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("webhook body: {e}"))?;
    let event = s(&v, "event").ok_or("webhook: no event")?;
    let payment = v.pointer("/payload/payment/entity").and_then(parse_payment);
    let order_entity = v.pointer("/payload/order/entity");
    let refund_entity = v.pointer("/payload/refund/entity");
    let order_id = payment
        .as_ref()
        .and_then(|p| p.order_id.clone())
        .or_else(|| order_entity.and_then(|o| s(o, "id")))
        .unwrap_or_default();
    let entity = refund_entity
        .and_then(|r| s(r, "id"))
        .or_else(|| payment.as_ref().map(|p| p.id.clone()))
        .or_else(|| order_entity.and_then(|o| s(o, "id")))
        .ok_or("webhook: no entity id")?;
    Ok(RzWebhook {
        event_id: format!("{event}:{entity}"),
        event,
        order_id,
        payment,
    })
}

fn retryable(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

fn is_duplicate_receipt(v: &Value) -> bool {
    let d = v
        .pointer("/error/description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    d.contains("duplicate") || d.contains("already been processed")
}

/// Problem for a non-retryable API answer (never echoes credentials).
fn api_error(what: &str, status: u16, v: &Value) -> KernelError {
    let code = v
        .pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or("UNKNOWN");
    let desc = v
        .pointer("/error/description")
        .and_then(Value::as_str)
        .unwrap_or("no description");
    match status {
        401 | 403 => KernelError::Unavailable(format!(
            "razorpay rejected the API credentials ({what}); check SUTRA_RAZORPAY_KEY_ID / key secret"
        )),
        404 => KernelError::NotFound {
            entity: "gateway resource",
        },
        400 | 422 => KernelError::BadRequest(format!("razorpay {what}: {code}: {desc}")),
        _ => KernelError::Unavailable(format!("razorpay {what}: HTTP {status} {code}")),
    }
}

fn unavailable(what: &str, last: &str, attempts: u32) -> KernelError {
    KernelError::Unavailable(format!(
        "razorpay {what}: {last} after {attempts} attempt(s)"
    ))
}

// ------------------------------------------------------------------ client

/// Razorpay API client.
#[derive(Clone, Debug)]
pub struct RazorpayClient {
    http: reqwest::Client,
    settings: RazorpaySettings,
}

impl RazorpayClient {
    /// Builds the HTTP client; does not connect.
    pub fn new(settings: RazorpaySettings) -> Result<Self, String> {
        url::Url::parse(&settings.base_url)
            .map_err(|e| format!("invalid SUTRA_RAZORPAY_BASE_URL: {e}"))?;
        if settings.key_id.is_empty() || settings.key_secret.is_empty() {
            return Err("razorpay key id and key secret are required".into());
        }
        if settings.webhook_secret.is_empty() {
            return Err("razorpay webhook secret is required".into());
        }
        let http = reqwest::Client::builder()
            .timeout(settings.timeout)
            .connect_timeout(settings.timeout.min(Duration::from_secs(5)))
            .build()
            .map_err(|e| format!("razorpay client: {e}"))?;
        Ok(Self { http, settings })
    }

    pub fn settings(&self) -> &RazorpaySettings {
        &self.settings
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.settings.base_url.trim_end_matches('/'))
    }

    async fn backoff(&self, attempt: u32) {
        let factor = 2u32.saturating_pow(attempt.saturating_sub(1).min(8));
        let d = self
            .settings
            .retry_backoff
            .saturating_mul(factor)
            .min(Duration::from_secs(5));
        tokio::time::sleep(d).await;
    }

    /// One request; `Err` = transport failure / timeout (retryable).
    async fn send_once(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
        refund_key: Option<&str>,
    ) -> Result<(u16, Value), String> {
        let mut r = self
            .http
            .request(method, self.url(path))
            .basic_auth(&self.settings.key_id, Some(&self.settings.key_secret))
            .header("accept", "application/json");
        if let Some(k) = refund_key {
            r = r.header("X-Refund-Idempotency", k);
        }
        if let Some(b) = body {
            r = r.json(b);
        }
        let classify = |e: &reqwest::Error| {
            if e.is_timeout() {
                "timeout".to_owned()
            } else if e.is_connect() {
                "connection failed".to_owned()
            } else {
                "transport error".to_owned()
            }
        };
        let resp = r.send().await.map_err(|e| classify(&e))?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await.map_err(|e| classify(&e))?;
        let v = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Ok((status, v))
    }

    /// GET with bounded retries.
    async fn get(&self, what: &str, path: &str) -> Result<Value, KernelError> {
        let attempts = self.settings.max_retries + 1;
        let mut last = String::new();
        for attempt in 0..attempts {
            if attempt > 0 {
                self.backoff(attempt).await;
            }
            match self.send_once(reqwest::Method::GET, path, None, None).await {
                Ok((st, v)) if (200..300).contains(&st) => return Ok(v),
                Ok((st, _)) if retryable(st) => last = format!("HTTP {st}"),
                Ok((st, v)) => return Err(api_error(what, st, &v)),
                Err(m) => last = m,
            }
        }
        Err(unavailable(what, &last, attempts))
    }

    /// The order created earlier with `receipt` (single attempt; `Err` = could not ask).
    pub async fn find_order_by_receipt(&self, receipt: &str) -> Result<Option<RzOrder>, String> {
        let path = format!("/v1/orders?receipt={receipt}");
        match self
            .send_once(reqwest::Method::GET, &path, None, None)
            .await?
        {
            (st, v) if (200..300).contains(&st) => Ok(items(&v)
                .iter()
                .filter_map(parse_order)
                .find(|o| o.receipt.as_deref() == Some(receipt))),
            (st, _) => Err(format!("HTTP {st}")),
        }
    }

    /// `POST /v1/orders`, idempotent on `receipt` (see the module docs).
    pub async fn create_order(
        &self,
        amount_paise: i64,
        currency: &str,
        receipt: &str,
        notes: Value,
    ) -> Result<RzOrder, KernelError> {
        if receipt.is_empty() || receipt.len() > MAX_RECEIPT_LEN {
            return Err(KernelError::BadRequest(
                "razorpay receipt must be 1-40 characters".into(),
            ));
        }
        let body = json!({
            "amount": amount_paise,
            "currency": currency,
            "receipt": receipt,
            "notes": notes,
        });
        let attempts = self.settings.max_retries + 1;
        let mut last = String::new();
        for attempt in 0..attempts {
            if attempt > 0 {
                self.backoff(attempt).await;
                // The previous POST may have created the order before its answer was lost.
                if let Ok(Some(o)) = self.find_order_by_receipt(receipt).await {
                    return Ok(o);
                }
            }
            match self
                .send_once(reqwest::Method::POST, "/v1/orders", Some(&body), None)
                .await
            {
                Ok((st, v)) if (200..300).contains(&st) => {
                    return parse_order(&v).ok_or_else(|| {
                        KernelError::Unavailable("razorpay create order: unreadable answer".into())
                    })
                }
                Ok((400, v)) if is_duplicate_receipt(&v) => {
                    return match self.find_order_by_receipt(receipt).await {
                        Ok(Some(o)) => Ok(o),
                        Ok(None) => Err(api_error("create order", 400, &v)),
                        Err(m) => Err(unavailable("create order", &m, attempt + 1)),
                    };
                }
                Ok((st, _)) if retryable(st) => last = format!("HTTP {st}"),
                Ok((st, v)) => return Err(api_error("create order", st, &v)),
                Err(m) => last = m,
            }
        }
        Err(unavailable("create order", &last, attempts))
    }

    /// `GET /v1/orders/{id}/payments`.
    pub async fn order_payments(&self, order_id: &str) -> Result<Vec<RzPayment>, KernelError> {
        if !valid_id("order_", order_id) {
            return Err(KernelError::BadRequest(format!(
                "not a razorpay order id: {order_id}"
            )));
        }
        let v = self
            .get("fetch payments", &format!("/v1/orders/{order_id}/payments"))
            .await?;
        Ok(items(&v).iter().filter_map(parse_payment).collect())
    }

    /// `GET /v1/payments/{id}/refunds`.
    pub async fn payment_refunds(&self, payment_id: &str) -> Result<Vec<RzRefund>, KernelError> {
        if !valid_id("pay_", payment_id) {
            return Err(KernelError::BadRequest(format!(
                "not a razorpay payment id: {payment_id}"
            )));
        }
        let v = self
            .get(
                "fetch refunds",
                &format!("/v1/payments/{payment_id}/refunds"),
            )
            .await?;
        Ok(items(&v).iter().filter_map(parse_refund).collect())
    }

    /// `POST /v1/payments/{id}/refund` with `X-Refund-Idempotency: key` and `receipt: key`
    /// (retried with the same key).
    pub async fn refund(
        &self,
        payment_id: &str,
        amount_paise: i64,
        key: &str,
    ) -> Result<RzRefund, KernelError> {
        if !valid_id("pay_", payment_id) {
            return Err(KernelError::BadRequest(format!(
                "not a razorpay payment id: {payment_id}"
            )));
        }
        let path = format!("/v1/payments/{payment_id}/refund");
        let body = json!({"amount": amount_paise, "speed": "normal", "receipt": key});
        let attempts = self.settings.max_retries + 1;
        let mut last = String::new();
        for attempt in 0..attempts {
            if attempt > 0 {
                self.backoff(attempt).await;
            }
            match self
                .send_once(reqwest::Method::POST, &path, Some(&body), Some(key))
                .await
            {
                Ok((st, v)) if (200..300).contains(&st) => {
                    return parse_refund(&v).ok_or_else(|| {
                        KernelError::Unavailable("razorpay refund: unreadable answer".into())
                    })
                }
                Ok((400, v)) if is_duplicate_receipt(&v) => {
                    let found = self
                        .payment_refunds(payment_id)
                        .await?
                        .into_iter()
                        .find(|r| r.receipt.as_deref() == Some(key));
                    return found.ok_or_else(|| api_error("refund", 400, &v));
                }
                Ok((st, _)) if retryable(st) => last = format!("HTTP {st}"),
                Ok((st, v)) => return Err(api_error("refund", st, &v)),
                Err(m) => last = m,
            }
        }
        Err(unavailable("refund", &last, attempts))
    }

    /// Verifies `X-Razorpay-Signature` over the raw body, then parses it (`Unauthenticated` on a
    /// bad signature, `BadRequest` on an unreadable signed body).
    pub fn verify_webhook(&self, body: &[u8], signature: &str) -> Result<RzWebhook, KernelError> {
        if !verify_hmac_hex(self.settings.webhook_secret.as_bytes(), body, signature) {
            return Err(KernelError::Unauthenticated);
        }
        parse_webhook(body).map_err(KernelError::BadRequest)
    }

    /// Verifies a checkout callback signature (`order_id|payment_id`, key secret).
    pub fn verify_checkout(&self, order_id: &str, payment_id: &str, signature: &str) -> bool {
        verify_hmac_hex(
            self.settings.key_secret.as_bytes(),
            format!("{order_id}|{payment_id}").as_bytes(),
            signature,
        )
    }
}

// ------------------------------------------------------------------ contract stub

/// A local HTTP stub of the Razorpay endpoints above, reproducing the documented request,
/// response and error formats (doc URLs in the module docs). Used by the contract tests here and
/// by the API's PostgreSQL tests (`db_p2_pay*.rs`, feature `razorpay-stub`). **Never** wired in
/// a running server: it is not a gateway.
#[cfg(any(test, feature = "razorpay-stub"))]
pub mod stub {
    use super::{checkout_signature, webhook_signature};
    use axum::extract::{Path, Query, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde_json::{json, Value};
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// A fault the stub injects into the next matching request.
    #[derive(Clone, Debug)]
    pub enum Fault {
        /// Answer with this status (Razorpay error body) without doing anything.
        Status(u16),
        /// Sleep before answering (longer than the client timeout = timeout).
        Delay(Duration),
        /// Do the work, then answer 503 (the answer is "lost").
        LoseAnswer,
    }

    /// How a simulated customer attempt ends.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Attempt {
        Captured,
        Authorized,
        Failed,
    }

    #[derive(Clone, Debug, Default)]
    pub struct StubState {
        pub orders: Vec<Value>,
        pub payments: Vec<Value>,
        pub refunds: Vec<Value>,
        /// `(method path, faults)`: e.g. `("POST /v1/orders", [..])`.
        pub faults: HashMap<String, VecDeque<Fault>>,
        /// Every request: `METHOD path`.
        pub log: Vec<String>,
        /// Refund idempotency keys → refund id.
        pub refund_keys: HashMap<String, String>,
        counter: u64,
    }

    /// The running stub.
    #[derive(Clone)]
    pub struct RazorpayStub {
        pub base_url: String,
        pub key_id: String,
        pub key_secret: String,
        pub webhook_secret: String,
        pub state: Arc<Mutex<StubState>>,
    }

    #[derive(Clone)]
    struct Shared {
        auth: String,
        state: Arc<Mutex<StubState>>,
    }

    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    fn base64(input: &[u8]) -> String {
        let mut out = String::new();
        for chunk in input.chunks(3) {
            let b = [
                chunk[0],
                chunk.get(1).copied().unwrap_or(0),
                chunk.get(2).copied().unwrap_or(0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for k in 0..4 {
                if k <= chunk.len() {
                    out.push(char::from(B64[((n >> (18 - 6 * k)) & 63) as usize]));
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    fn error(
        status: u16,
        code: &str,
        description: &str,
        field: Option<&str>,
    ) -> (StatusCode, Json<Value>) {
        (
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST),
            Json(json!({"error": {
                "code": code,
                "description": description,
                "source": "business",
                "step": "payment_initiation",
                "reason": "input_validation_failed",
                "metadata": {},
                "field": field,
            }})),
        )
    }

    type Reply = (StatusCode, Json<Value>);

    impl Shared {
        /// Basic auth + fault injection + log. `Err` = answer now.
        async fn enter(&self, route: &str, headers: &HeaderMap) -> Result<bool, Reply> {
            let fault = {
                let mut st = self
                    .state
                    .lock()
                    .map_err(|_| error(500, "SERVER_ERROR", "poisoned", None))?;
                st.log.push(route.to_owned());
                st.faults.get_mut(route).and_then(VecDeque::pop_front)
            };
            let auth = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            if auth != self.auth {
                return Err(error(
                    401,
                    "BAD_REQUEST_ERROR",
                    "The API key/secret provided is invalid.",
                    None,
                ));
            }
            match fault {
                Some(Fault::Status(s)) => Err(error(s, "SERVER_ERROR", "injected fault", None)),
                Some(Fault::Delay(d)) => {
                    tokio::time::sleep(d).await;
                    Ok(false)
                }
                Some(Fault::LoseAnswer) => Ok(true),
                None => Ok(false),
            }
        }
    }

    fn lost() -> Reply {
        error(503, "SERVER_ERROR", "answer lost (injected)", None)
    }

    async fn create_order(
        State(sh): State<Shared>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Reply {
        let lose = match sh.enter("POST /v1/orders", &headers).await {
            Ok(l) => l,
            Err(r) => return r,
        };
        let Some(amount) = body.get("amount").and_then(Value::as_i64) else {
            return error(
                400,
                "BAD_REQUEST_ERROR",
                "The amount must be an integer.",
                Some("amount"),
            );
        };
        if amount < 100 {
            return error(
                400,
                "BAD_REQUEST_ERROR",
                "The amount must be at least INR 1.00",
                Some("amount"),
            );
        }
        let receipt = body
            .get("receipt")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if receipt.len() > 40 {
            return error(
                400,
                "BAD_REQUEST_ERROR",
                "receipt: the length must be no more than 40.",
                Some("receipt"),
            );
        }
        let Ok(mut st) = sh.state.lock() else {
            return error(500, "SERVER_ERROR", "poisoned", None);
        };
        if !receipt.is_empty() && st.orders.iter().any(|o| o["receipt"] == receipt.as_str()) {
            return error(
                400,
                "BAD_REQUEST_ERROR",
                "Duplicate request. This request has already been processed.",
                Some("receipt"),
            );
        }
        st.counter += 1;
        let order = json!({
            "id": format!("order_Stub{:010}", st.counter),
            "entity": "order",
            "amount": amount,
            "amount_paid": 0,
            "amount_due": amount,
            "currency": body.get("currency").and_then(Value::as_str).unwrap_or("INR"),
            "receipt": if receipt.is_empty() { Value::Null } else { Value::from(receipt) },
            "offer_id": null,
            "status": "created",
            "attempts": 0,
            "notes": body.get("notes").cloned().unwrap_or(json!([])),
            "created_at": 1_700_000_000 + st.counter,
        });
        st.orders.push(order.clone());
        if lose {
            return lost();
        }
        (StatusCode::OK, Json(order))
    }

    async fn list_orders(
        State(sh): State<Shared>,
        headers: HeaderMap,
        Query(q): Query<HashMap<String, String>>,
    ) -> Reply {
        if let Err(r) = sh.enter("GET /v1/orders", &headers).await {
            return r;
        }
        let Ok(st) = sh.state.lock() else {
            return error(500, "SERVER_ERROR", "poisoned", None);
        };
        let items: Vec<Value> = st
            .orders
            .iter()
            .filter(|o| match q.get("receipt") {
                Some(r) => o["receipt"] == r.as_str(),
                None => true,
            })
            .cloned()
            .collect();
        (
            StatusCode::OK,
            Json(json!({"entity": "collection", "count": items.len(), "items": items})),
        )
    }

    async fn order_payments(
        State(sh): State<Shared>,
        headers: HeaderMap,
        Path(order): Path<String>,
    ) -> Reply {
        if let Err(r) = sh.enter("GET /v1/orders/payments", &headers).await {
            return r;
        }
        let Ok(st) = sh.state.lock() else {
            return error(500, "SERVER_ERROR", "poisoned", None);
        };
        if !st.orders.iter().any(|o| o["id"] == order.as_str()) {
            return error(
                400,
                "BAD_REQUEST_ERROR",
                "The id provided does not exist",
                Some("id"),
            );
        }
        let items: Vec<Value> = st
            .payments
            .iter()
            .filter(|p| p["order_id"] == order.as_str())
            .cloned()
            .collect();
        (
            StatusCode::OK,
            Json(json!({"entity": "collection", "count": items.len(), "items": items})),
        )
    }

    async fn refund(
        State(sh): State<Shared>,
        headers: HeaderMap,
        Path(payment): Path<String>,
        Json(body): Json<Value>,
    ) -> Reply {
        let lose = match sh.enter("POST /v1/payments/refund", &headers).await {
            Ok(l) => l,
            Err(r) => return r,
        };
        let key = headers
            .get("x-refund-idempotency")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let Ok(mut st) = sh.state.lock() else {
            return error(500, "SERVER_ERROR", "poisoned", None);
        };
        if let Some(k) = &key {
            if k.len() < 10 {
                return error(
                    400,
                    "BAD_REQUEST_ERROR",
                    "The idempotency key must be at least 10 characters long.",
                    None,
                );
            }
            if let Some(id) = st.refund_keys.get(k).cloned() {
                if let Some(r) = st.refunds.iter().find(|r| r["id"] == id.as_str()) {
                    return (StatusCode::OK, Json(r.clone()));
                }
            }
        }
        let Some(p) = st
            .payments
            .iter()
            .find(|p| p["id"] == payment.as_str())
            .cloned()
        else {
            return error(
                400,
                "BAD_REQUEST_ERROR",
                "The id provided does not exist",
                Some("id"),
            );
        };
        if p["status"] != "captured" {
            return error(
                400,
                "BAD_REQUEST_ERROR",
                "The payment status should be captured for action to be taken.",
                None,
            );
        }
        let captured = p["amount"].as_i64().unwrap_or(0);
        let refunded: i64 = st
            .refunds
            .iter()
            .filter(|r| r["payment_id"] == payment.as_str())
            .filter_map(|r| r["amount"].as_i64())
            .sum();
        let amount = body
            .get("amount")
            .and_then(Value::as_i64)
            .unwrap_or(captured - refunded);
        if amount + refunded > captured {
            return error(
                400,
                "BAD_REQUEST_ERROR",
                "The refund amount provided is greater than amount captured.",
                Some("amount"),
            );
        }
        st.counter += 1;
        let r = json!({
            "id": format!("rfnd_Stub{:010}", st.counter),
            "entity": "refund",
            "amount": amount,
            "currency": "INR",
            "payment_id": payment,
            "notes": [],
            "receipt": body.get("receipt").cloned().unwrap_or(Value::Null),
            "acquirer_data": {"arn": null},
            "created_at": 1_700_000_000 + st.counter,
            "batch_id": null,
            "status": "processed",
            "speed_processed": "normal",
            "speed_requested": "normal",
        });
        st.refunds.push(r.clone());
        if let Some(k) = key {
            if let Some(id) = r["id"].as_str() {
                st.refund_keys.insert(k, id.to_owned());
            }
        }
        if lose {
            return lost();
        }
        (StatusCode::OK, Json(r))
    }

    async fn payment_refunds(
        State(sh): State<Shared>,
        headers: HeaderMap,
        Path(payment): Path<String>,
    ) -> Reply {
        if let Err(r) = sh.enter("GET /v1/payments/refunds", &headers).await {
            return r;
        }
        let Ok(st) = sh.state.lock() else {
            return error(500, "SERVER_ERROR", "poisoned", None);
        };
        let items: Vec<Value> = st
            .refunds
            .iter()
            .filter(|r| r["payment_id"] == payment.as_str())
            .cloned()
            .collect();
        (
            StatusCode::OK,
            Json(json!({"entity": "collection", "count": items.len(), "items": items})),
        )
    }

    impl RazorpayStub {
        /// Starts the stub on `127.0.0.1:<random port>` (runs until the runtime stops).
        pub async fn start() -> std::io::Result<Self> {
            let key_id = "rzp_test_StubKey000001".to_owned();
            let key_secret = "stub-key-secret-0123456789abcdef".to_owned();
            let webhook_secret = "stub-webhook-secret-0123456789abcdef".to_owned();
            let state = Arc::new(Mutex::new(StubState::default()));
            let shared = Shared {
                auth: format!(
                    "Basic {}",
                    base64(format!("{key_id}:{key_secret}").as_bytes())
                ),
                state: state.clone(),
            };
            let app = Router::new()
                .route("/v1/orders", post(create_order).get(list_orders))
                .route("/v1/orders/{id}/payments", get(order_payments))
                .route("/v1/payments/{id}/refund", post(refund))
                .route("/v1/payments/{id}/refunds", get(payment_refunds))
                .with_state(shared);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let addr = listener.local_addr()?;
            tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            Ok(Self {
                base_url: format!("http://{addr}"),
                key_id,
                key_secret,
                webhook_secret,
                state,
            })
        }

        /// Injects `fault` into the next request of `route` (`"POST /v1/orders"`,
        /// `"GET /v1/orders"`, `"GET /v1/orders/payments"`, `"POST /v1/payments/refund"`,
        /// `"GET /v1/payments/refunds"`).
        pub fn inject(&self, route: &str, fault: Fault) {
            if let Ok(mut st) = self.state.lock() {
                st.faults
                    .entry(route.to_owned())
                    .or_default()
                    .push_back(fault);
            }
        }

        /// Number of requests of `route` seen so far.
        pub fn calls(&self, route: &str) -> usize {
            self.state
                .lock()
                .map(|st| st.log.iter().filter(|l| l.as_str() == route).count())
                .unwrap_or(0)
        }

        /// Number of orders created.
        pub fn orders(&self) -> usize {
            self.state.lock().map(|st| st.orders.len()).unwrap_or(0)
        }

        /// A customer attempt on `order_id` (amount = the order's); returns the payment entity.
        pub fn pay(&self, order_id: &str, attempt: Attempt) -> Value {
            let Ok(mut st) = self.state.lock() else {
                return Value::Null;
            };
            let amount = st
                .orders
                .iter()
                .find(|o| o["id"] == order_id)
                .and_then(|o| o["amount"].as_i64())
                .unwrap_or(0);
            st.counter += 1;
            let (status, captured, code, desc) = match attempt {
                Attempt::Captured => ("captured", true, Value::Null, Value::Null),
                Attempt::Authorized => ("authorized", false, Value::Null, Value::Null),
                Attempt::Failed => (
                    "failed",
                    false,
                    Value::from("BAD_REQUEST_ERROR"),
                    Value::from("Payment failed"),
                ),
            };
            // Shape of https://razorpay.com/docs/api/orders/fetch-payments/ (items[]).
            let p = json!({
                "id": format!("pay_Stub{:010}", st.counter),
                "entity": "payment",
                "amount": amount,
                "currency": "INR",
                "status": status,
                "order_id": order_id,
                "invoice_id": null,
                "international": false,
                "method": "upi",
                "amount_refunded": 0,
                "refund_status": null,
                "captured": captured,
                "description": null,
                "card_id": null,
                "bank": null,
                "wallet": null,
                "vpa": "success@razorpay",
                "email": "void@razorpay.com",
                "contact": "+919999999999",
                "notes": [],
                "fee": if captured { Value::from(amount * 2 / 100) } else { Value::Null },
                "tax": if captured { Value::from(0) } else { Value::Null },
                "error_code": code,
                "error_description": desc,
                "error_source": null,
                "error_step": null,
                "error_reason": null,
                "acquirer_data": {"rrn": null},
                "created_at": 1_700_000_000 + st.counter,
            });
            st.payments.push(p.clone());
            if let Some(o) = st.orders.iter_mut().find(|o| o["id"] == order_id) {
                o["attempts"] = Value::from(o["attempts"].as_i64().unwrap_or(0) + 1);
                o["status"] = Value::from(if captured { "paid" } else { "attempted" });
            }
            p
        }

        /// Turns an `authorized` payment into `captured` (auto-capture finishing later).
        pub fn capture(&self, payment_id: &str) {
            if let Ok(mut st) = self.state.lock() {
                if let Some(p) = st.payments.iter_mut().find(|p| p["id"] == payment_id) {
                    p["status"] = Value::from("captured");
                    p["captured"] = Value::from(true);
                }
            }
        }

        /// Raw webhook body of `event` for `payment` (shape of
        /// https://razorpay.com/docs/webhooks/payments/ — `payment.captured`, `payment.failed`,
        /// `order.paid`) and its `X-Razorpay-Signature`.
        pub fn webhook(&self, event: &str, payment: &Value) -> (String, String) {
            let mut payload = json!({"payment": {"entity": payment}});
            let mut contains = vec!["payment"];
            if event == "order.paid" {
                let order = payment["order_id"].as_str().unwrap_or_default().to_owned();
                let o = self
                    .state
                    .lock()
                    .ok()
                    .and_then(|st| {
                        st.orders
                            .iter()
                            .find(|x| x["id"] == order.as_str())
                            .cloned()
                    })
                    .unwrap_or(Value::Null);
                payload["order"] = json!({"entity": o});
                contains.push("order");
            }
            let body = json!({
                "entity": "event",
                "account_id": "acc_BFQ7uQEaa7j2z7",
                "event": event,
                "contains": contains,
                "payload": payload,
                "created_at": 1_700_000_100,
            })
            .to_string();
            let sig = webhook_signature(&self.webhook_secret, body.as_bytes());
            (body, sig)
        }

        /// `razorpay_signature` Checkout would return for `(order, payment)`.
        pub fn checkout_signature(&self, order_id: &str, payment_id: &str) -> String {
            checkout_signature(&self.key_secret, order_id, payment_id)
        }

        /// Client settings pointing at this stub.
        pub fn settings(&self, timeout: Duration, max_retries: u32) -> super::RazorpaySettings {
            super::RazorpaySettings {
                base_url: self.base_url.clone(),
                key_id: self.key_id.clone(),
                key_secret: self.key_secret.clone(),
                webhook_secret: self.webhook_secret.clone(),
                timeout,
                max_retries,
                retry_backoff: Duration::from_millis(10),
            }
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn base64_matches_rfc4648() {
            assert_eq!(super::base64(b""), "");
            assert_eq!(super::base64(b"f"), "Zg==");
            assert_eq!(super::base64(b"fo"), "Zm8=");
            assert_eq!(super::base64(b"foo"), "Zm9v");
            assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::stub::{Attempt, Fault, RazorpayStub};
    use super::*;

    // https://razorpay.com/docs/webhooks/payments/ — `payment.captured` (netbanking sample),
    // trimmed to the fields the adapter reads plus a few it must ignore.
    const CAPTURED: &str = r#"{"entity":"event","account_id":"acc_BFQ7uQEaa7j2z7","event":"payment.captured","contains":["payment"],"payload":{"payment":{"entity":{"id":"pay_DESlfW9H8K9uqM","entity":"payment","amount":100,"currency":"INR","base_amount":100,"status":"captured","order_id":"order_DESlLckIVRkHWj","invoice_id":null,"international":false,"method":"netbanking","amount_refunded":0,"amount_transferred":0,"refund_status":null,"captured":true,"description":null,"card_id":null,"bank":"HDFC","wallet":null,"vpa":null,"email":"<email>","contact":"<phone>","notes":[],"fee":2,"tax":0,"error_code":null,"error_description":null,"error_source":null,"error_step":null,"error_reason":null,"acquirer_data":{"bank_transaction_id":"0125836177"},"created_at":1567674599}}},"created_at":1567674606}"#;

    // https://razorpay.com/docs/webhooks/payments/ — `payment.failed`.
    const FAILED: &str = r#"{"entity":"event","account_id":"acc_BFQ7uQEaa7j2z7","event":"payment.failed","contains":["payment"],"payload":{"payment":{"entity":{"id":"pay_DEAX6wUf7yVjXQ","entity":"payment","amount":100,"currency":"INR","status":"failed","order_id":"order_DEATVTRRctwEGb","captured":false,"error_code":"BAD_REQUEST_ERROR","error_description":"Payment failed","error_source":"bank","error_step":"payment_authorization","error_reason":"payment_failed","created_at":1567500434}}},"created_at":1567500436}"#;

    const SECRET: &str = "webhook-secret-0123456789abcdef0123";

    fn settings_for(stub: &RazorpayStub, timeout_ms: u64, retries: u32) -> RazorpaySettings {
        stub.settings(Duration::from_millis(timeout_ms), retries)
    }

    #[test]
    fn webhook_signature_is_hmac_sha256_of_the_raw_body() {
        // RFC 4231 test case 2 ("Jefe" / "what do ya want for nothing?").
        assert_eq!(
            hmac_hex(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        let sig = webhook_signature(SECRET, CAPTURED.as_bytes());
        assert!(verify_hmac_hex(
            SECRET.as_bytes(),
            CAPTURED.as_bytes(),
            &sig
        ));
        assert!(verify_hmac_hex(
            SECRET.as_bytes(),
            CAPTURED.as_bytes(),
            &sig.to_uppercase()
        ));
        // Tampered body, wrong secret, truncated / non-hex / empty signatures.
        let tampered = CAPTURED.replace("\"amount\":100,", "\"amount\":1,");
        assert!(!verify_hmac_hex(
            SECRET.as_bytes(),
            tampered.as_bytes(),
            &sig
        ));
        assert!(!verify_hmac_hex(b"other", CAPTURED.as_bytes(), &sig));
        assert!(!verify_hmac_hex(
            SECRET.as_bytes(),
            CAPTURED.as_bytes(),
            &sig[..63]
        ));
        assert!(!verify_hmac_hex(
            SECRET.as_bytes(),
            CAPTURED.as_bytes(),
            &"zz".repeat(32)
        ));
        assert!(!verify_hmac_hex(SECRET.as_bytes(), CAPTURED.as_bytes(), ""));
        assert!(!verify_hmac_hex(b"", CAPTURED.as_bytes(), &sig));
    }

    #[test]
    fn webhooks_parse_with_a_body_derived_event_id() {
        let w = parse_webhook(CAPTURED.as_bytes()).unwrap();
        assert_eq!(w.event, "payment.captured");
        assert_eq!(w.event_id, "payment.captured:pay_DESlfW9H8K9uqM");
        assert_eq!(w.order_id, "order_DESlLckIVRkHWj");
        let p = w.payment.unwrap();
        assert_eq!(
            (p.amount, p.captured, p.status.as_str()),
            (100, true, "captured")
        );
        let f = parse_webhook(FAILED.as_bytes()).unwrap();
        assert_eq!(f.event_id, "payment.failed:pay_DEAX6wUf7yVjXQ");
        assert_eq!(
            f.payment.and_then(|p| p.error_code).as_deref(),
            Some("BAD_REQUEST_ERROR")
        );
        let refund = r#"{"entity":"event","event":"refund.processed","contains":["refund","payment"],"payload":{"refund":{"entity":{"id":"rfnd_X1","payment_id":"pay_Y1","amount":100,"status":"processed"}}}}"#;
        let r = parse_webhook(refund.as_bytes()).unwrap();
        assert_eq!(r.event_id, "refund.processed:rfnd_X1");
        assert_eq!(r.order_id, "");
        assert!(parse_webhook(b"not json").is_err());
        assert!(parse_webhook(br#"{"event":"payment.captured","payload":{}}"#).is_err());
    }

    #[test]
    fn checkout_signature_is_order_pipe_payment() {
        // Same construction as the docs' `hmac_sha256(order_id + "|" + razorpay_payment_id, secret)`.
        let sig = checkout_signature("key-secret", "order_A1", "pay_B2");
        assert_eq!(sig, hmac_hex(b"key-secret", b"order_A1|pay_B2"));
        assert!(verify_hmac_hex(b"key-secret", b"order_A1|pay_B2", &sig));
        assert!(!verify_hmac_hex(b"key-secret", b"order_A1|pay_B3", &sig));
    }

    #[test]
    fn derived_keys_fit_razorpay_limits() {
        let r = receipt_for("tenant:account:some-very-long-idempotency-key-0123456789");
        assert!(r.len() <= MAX_RECEIPT_LEN && r.starts_with("sutra_"));
        assert_eq!(
            r,
            receipt_for("tenant:account:some-very-long-idempotency-key-0123456789")
        );
        assert_ne!(r, receipt_for("other"));
        let k = refund_key_for("refund:1:1");
        assert!(k.len() >= 10 && k.len() <= 40);
        assert!(k
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert!(valid_id("order_", "order_DESlLckIVRkHWj"));
        assert!(!valid_id("order_", "order_../x"));
        assert!(!valid_id("order_", "order_"));
        assert!(!valid_id("pay_", "order_X"));
    }

    #[test]
    fn outcomes_prefer_capture_then_in_flight() {
        let p = |status: &str, captured: bool| RzPayment {
            id: format!("pay_{status}"),
            order_id: Some("order_X".into()),
            amount: 100,
            currency: "INR".into(),
            status: status.into(),
            captured,
            error_code: None,
            error_description: None,
        };
        assert_eq!(outcome_of(&[]), RzOutcome::Pending);
        assert!(matches!(
            outcome_of(&[p("failed", false), p("captured", true)]),
            RzOutcome::Captured(_)
        ));
        assert!(matches!(
            outcome_of(&[p("refunded", true)]),
            RzOutcome::Captured(_)
        ));
        assert_eq!(
            outcome_of(&[p("failed", false), p("authorized", false)]),
            RzOutcome::Pending
        );
        assert!(matches!(
            outcome_of(&[p("failed", false)]),
            RzOutcome::Failed(_)
        ));
    }

    #[test]
    fn settings_debug_redacts_secrets() {
        let s = RazorpaySettings {
            base_url: LIVE_BASE_URL.into(),
            key_id: "rzp_live_X".into(),
            key_secret: "very-secret-key".into(),
            webhook_secret: "very-secret-hook".into(),
            timeout: Duration::from_secs(1),
            max_retries: 1,
            retry_backoff: Duration::from_millis(1),
        };
        let d = format!("{s:?}");
        assert!(!d.contains("very-secret"), "{d}");
        assert!(d.contains("rzp_live_X"));
    }

    // ---- contract tests against the local stub (real HTTP, real basic auth)

    #[tokio::test]
    async fn orders_are_created_idempotently_on_the_receipt() {
        let stub = RazorpayStub::start().await.unwrap();
        let c = RazorpayClient::new(settings_for(&stub, 2000, 2)).unwrap();
        let receipt = receipt_for("k1");
        let a = c
            .create_order(150_000, "INR", &receipt, json!({}))
            .await
            .unwrap();
        assert!(valid_id("order_", &a.id));
        assert_eq!(
            (a.amount, a.currency.as_str(), a.status.as_str()),
            (150_000, "INR", "created")
        );
        // A second create with the same receipt (our retry after a crash) returns the same order.
        let b = c
            .create_order(150_000, "INR", &receipt, json!({}))
            .await
            .unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(stub.orders(), 1);
        // Below the minimum → Razorpay's 400 surfaces as BadRequest.
        assert!(matches!(
            c.create_order(99, "INR", &receipt_for("k2"), json!({}))
                .await,
            Err(KernelError::BadRequest(_))
        ));
    }

    #[tokio::test]
    async fn lost_answers_and_5xx_are_retried_without_a_second_order() {
        let stub = RazorpayStub::start().await.unwrap();
        let c = RazorpayClient::new(settings_for(&stub, 2000, 2)).unwrap();
        // The order is created but the answer is lost → the retry finds it by receipt.
        stub.inject("POST /v1/orders", Fault::LoseAnswer);
        let o = c
            .create_order(10_000, "INR", &receipt_for("lost"), json!({}))
            .await
            .unwrap();
        assert_eq!(stub.orders(), 1);
        assert_eq!(o.receipt.as_deref(), Some(receipt_for("lost").as_str()));
        // A plain 503 then success.
        stub.inject("POST /v1/orders", Fault::Status(503));
        c.create_order(10_000, "INR", &receipt_for("503"), json!({}))
            .await
            .unwrap();
        assert_eq!(stub.orders(), 2);
        // 503 on every attempt → Unavailable after 1 + max_retries POSTs.
        for _ in 0..3 {
            stub.inject("POST /v1/orders", Fault::Status(503));
        }
        let before = stub.calls("POST /v1/orders");
        assert!(matches!(
            c.create_order(10_000, "INR", &receipt_for("down"), json!({}))
                .await,
            Err(KernelError::Unavailable(_))
        ));
        assert_eq!(stub.calls("POST /v1/orders") - before, 3);
    }

    #[tokio::test]
    async fn timeouts_are_bounded_and_retried() {
        let stub = RazorpayStub::start().await.unwrap();
        let c = RazorpayClient::new(settings_for(&stub, 200, 1)).unwrap();
        let o = c
            .create_order(10_000, "INR", &receipt_for("t"), json!({}))
            .await
            .unwrap();
        // First fetch times out (stub sleeps past the client timeout), the retry answers.
        stub.inject(
            "GET /v1/orders/payments",
            Fault::Delay(Duration::from_millis(600)),
        );
        let started = std::time::Instant::now();
        assert!(c.order_payments(&o.id).await.unwrap().is_empty());
        assert!(started.elapsed() < Duration::from_secs(3));
        // Both attempts time out → Unavailable (never a guessed outcome).
        stub.inject(
            "GET /v1/orders/payments",
            Fault::Delay(Duration::from_millis(600)),
        );
        stub.inject(
            "GET /v1/orders/payments",
            Fault::Delay(Duration::from_millis(600)),
        );
        assert!(matches!(
            c.order_payments(&o.id).await,
            Err(KernelError::Unavailable(m)) if m.contains("timeout")
        ));
    }

    #[tokio::test]
    async fn order_payments_drive_the_outcome() {
        let stub = RazorpayStub::start().await.unwrap();
        let c = RazorpayClient::new(settings_for(&stub, 2000, 0)).unwrap();
        let o = c
            .create_order(50_000, "INR", &receipt_for("p"), json!({}))
            .await
            .unwrap();
        assert_eq!(
            outcome_of(&c.order_payments(&o.id).await.unwrap()),
            RzOutcome::Pending
        );
        stub.pay(&o.id, Attempt::Failed);
        assert!(matches!(
            outcome_of(&c.order_payments(&o.id).await.unwrap()),
            RzOutcome::Failed(_)
        ));
        let auth = stub.pay(&o.id, Attempt::Authorized);
        assert_eq!(
            outcome_of(&c.order_payments(&o.id).await.unwrap()),
            RzOutcome::Pending
        );
        stub.capture(auth["id"].as_str().unwrap());
        match outcome_of(&c.order_payments(&o.id).await.unwrap()) {
            RzOutcome::Captured(p) => {
                assert_eq!(p.id, auth["id"].as_str().unwrap());
                assert_eq!(p.amount, 50_000);
            }
            other => panic!("expected a capture, got {other:?}"),
        }
        assert!(matches!(
            c.order_payments("order_../../v1/payments").await,
            Err(KernelError::BadRequest(_))
        ));
    }

    #[tokio::test]
    async fn wrong_credentials_are_unavailable_not_leaked() {
        let stub = RazorpayStub::start().await.unwrap();
        let mut s = settings_for(&stub, 2000, 0);
        s.key_secret = "wrong-secret-value".into();
        let c = RazorpayClient::new(s).unwrap();
        match c
            .create_order(10_000, "INR", &receipt_for("x"), json!({}))
            .await
        {
            Err(KernelError::Unavailable(m)) => assert!(!m.contains("wrong-secret-value"), "{m}"),
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn refunds_are_idempotent_on_the_key() {
        let stub = RazorpayStub::start().await.unwrap();
        let c = RazorpayClient::new(settings_for(&stub, 2000, 2)).unwrap();
        let o = c
            .create_order(50_000, "INR", &receipt_for("r"), json!({}))
            .await
            .unwrap();
        let p = stub.pay(&o.id, Attempt::Captured);
        let pid = p["id"].as_str().unwrap();
        let key = refund_key_for("refund:abc:1");
        // The refund is made but its answer is lost: the retry with the same key returns it.
        stub.inject("POST /v1/payments/refund", Fault::LoseAnswer);
        let r = c.refund(pid, 20_000, &key).await.unwrap();
        assert_eq!((r.amount, r.status.as_str()), (20_000, "processed"));
        let again = c.refund(pid, 20_000, &key).await.unwrap();
        assert_eq!(r.id, again.id);
        assert_eq!(c.payment_refunds(pid).await.unwrap().len(), 1);
        // More than captured → 400 BadRequest.
        assert!(matches!(
            c.refund(pid, 40_000, &refund_key_for("refund:abc:2")).await,
            Err(KernelError::BadRequest(_))
        ));
    }

    #[tokio::test]
    async fn stub_webhooks_and_checkout_signatures_verify() {
        let stub = RazorpayStub::start().await.unwrap();
        let c = RazorpayClient::new(settings_for(&stub, 2000, 0)).unwrap();
        let o = c
            .create_order(50_000, "INR", &receipt_for("w"), json!({}))
            .await
            .unwrap();
        let p = stub.pay(&o.id, Attempt::Captured);
        let pid = p["id"].as_str().unwrap();
        for event in ["payment.captured", "order.paid"] {
            let (body, sig) = stub.webhook(event, &p);
            let w = c.verify_webhook(body.as_bytes(), &sig).unwrap();
            assert_eq!(w.order_id, o.id);
            assert_eq!(w.event_id, format!("{event}:{pid}"));
            let tampered = body.replace("50000", "5000");
            assert!(matches!(
                c.verify_webhook(tampered.as_bytes(), &sig),
                Err(KernelError::Unauthenticated)
            ));
        }
        let sig = stub.checkout_signature(&o.id, pid);
        assert!(c.verify_checkout(&o.id, pid, &sig));
        assert!(!c.verify_checkout(&o.id, "pay_Other", &sig));
    }
}
