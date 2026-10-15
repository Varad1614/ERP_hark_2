//! Deterministic dev / test payment gateway (Razorpay-shaped) behind
//! [`PaymentGatewayPort`] — **seeded by the foundation, owned and extended by 5B**.
//!
//! * Orders: `order_ref = "order_mock_" + sha256(idempotency_key)[..14]` — the same key always
//!   returns the same order (idempotent create).
//! * Outcome (`fetch_outcome`) is a pure function of the amount's paise so tests and demos can
//!   force every branch: `.13` → `Failed` (card declined), `.99` → `Pending` (timeout / in
//!   flight), anything else → `Captured` with `payment_ref = "pay_mock_" + …`.
//! * Webhooks: body = JSON `{"id", "event", "orderId", "paymentId", "amount", "code"}`,
//!   signature = lowercase hex HMAC-SHA256 of the raw body with the adapter secret
//!   ([`MockPaymentGateway::sign`]); `verify_webhook` checks it in constant time.
//! * Refunds: `refund_ref = "rfnd_mock_" + sha256(key)[..14]`, always `Processed` unless the
//!   amount's paise are `.13` (`Failed`) or `.99` (`Pending`).
//! * **5B extension:** an in-memory order book (order ref → amount) so [`fetch_outcome`] answers
//!   for orders this process created (the payment's "return from checkout" and the admin
//!   recovery of a pending payment, L3-F-010). Unknown orders (e.g. after a restart) stay
//!   `Pending` until a signed webhook or an explicit expiry resolves them.
//!
//! [`fetch_outcome`]: PaymentGatewayPort::fetch_outcome

use hmac::{Hmac, Mac};
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use sutra_contracts::finance::ports::{
    GatewayOrder, GatewayOrderRequest, GatewayOrderStatus, GatewayPaymentOutcome, GatewayRefund,
    GatewayRefundRequest, GatewayRefundStatus, GatewayWebhook, PaymentGatewayPort,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

/// Development webhook secret (never used outside dev / tests).
pub const DEV_WEBHOOK_SECRET: &str = "sutra-dev-gateway-secret";

/// The mock adapter.
#[derive(Clone, Debug)]
pub struct MockPaymentGateway {
    secret: String,
    /// Orders created by this adapter instance (order ref → amount) — 5B.
    orders: Arc<Mutex<HashMap<String, Decimal>>>,
}

impl Default for MockPaymentGateway {
    fn default() -> Self {
        Self::new(DEV_WEBHOOK_SECRET)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn digest(prefix: &str, key: &str) -> String {
    let h = Sha256::digest(key.as_bytes());
    format!("{prefix}{}", &hex(&h)[..14])
}

/// Paise of an amount (`12.13` → 13).
fn paise(amount: Decimal) -> u32 {
    let frac = (amount.fract() * Decimal::from(100)).round();
    u32::try_from(frac.mantissa().unsigned_abs() % 100).unwrap_or(0)
}

impl MockPaymentGateway {
    pub fn new(secret: impl Into<String>) -> Self {
        Self {
            secret: secret.into(),
            orders: Arc::default(),
        }
    }

    /// A signed-body helper for tests and demos: the JSON body of a `payment.captured` /
    /// `payment.failed` webhook (`amount` as a decimal string).
    pub fn webhook_body(event_id: &str, event: &str, order_ref: &str, amount: Decimal) -> String {
        serde_json::json!({
            "id": event_id,
            "event": event,
            "orderId": order_ref,
            "paymentId": digest("pay_mock_", order_ref),
            "amount": crate::shared::money::fmt(amount),
            "code": if event == "payment.failed" { Some("BAD_REQUEST_ERROR") } else { None },
        })
        .to_string()
    }

    /// Lowercase hex HMAC-SHA256 of `body` (what the gateway sends as `X-Razorpay-Signature`).
    pub fn sign(&self, body: &[u8]) -> String {
        let mut mac = match Hmac::<Sha256>::new_from_slice(self.secret.as_bytes()) {
            Ok(m) => m,
            Err(_) => return String::new(),
        };
        mac.update(body);
        hex(&mac.finalize().into_bytes())
    }

    /// The deterministic outcome of an order of `amount`.
    pub fn outcome_for(order_ref: &str, amount: Decimal) -> GatewayPaymentOutcome {
        match paise(amount) {
            13 => GatewayPaymentOutcome::Failed {
                code: "BAD_REQUEST_ERROR".into(),
                reason: "card declined (mock)".into(),
            },
            99 => GatewayPaymentOutcome::Pending,
            _ => GatewayPaymentOutcome::Captured {
                payment_ref: digest("pay_mock_", order_ref),
                amount,
            },
        }
    }

    fn parse_webhook(body: &[u8]) -> Result<GatewayWebhook, KernelError> {
        let v: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| KernelError::BadRequest(format!("webhook body: {e}")))?;
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_owned);
        let event =
            s("event").ok_or_else(|| KernelError::BadRequest("webhook: no event".into()))?;
        let order_ref =
            s("orderId").ok_or_else(|| KernelError::BadRequest("webhook: no orderId".into()))?;
        let event_id = s("id").ok_or_else(|| KernelError::BadRequest("webhook: no id".into()))?;
        let outcome = match event.as_str() {
            "payment.captured" => {
                let amount = s("amount")
                    .and_then(|a| a.parse::<Decimal>().ok())
                    .ok_or_else(|| KernelError::BadRequest("webhook: bad amount".into()))?;
                GatewayPaymentOutcome::Captured {
                    payment_ref: s("paymentId")
                        .ok_or_else(|| KernelError::BadRequest("webhook: no paymentId".into()))?,
                    amount,
                }
            }
            "payment.failed" => GatewayPaymentOutcome::Failed {
                code: s("code").unwrap_or_else(|| "PAYMENT_FAILED".into()),
                reason: "payment failed (mock webhook)".into(),
            },
            _ => GatewayPaymentOutcome::Pending,
        };
        Ok(GatewayWebhook {
            event_id,
            event,
            order_ref,
            outcome,
        })
    }
}

#[async_trait::async_trait]
impl PaymentGatewayPort for MockPaymentGateway {
    fn name(&self) -> &'static str {
        "mock"
    }

    async fn create_order(
        &self,
        _scope: &ExecutionScope,
        request: GatewayOrderRequest,
    ) -> Result<GatewayOrder, KernelError> {
        if request.amount <= Decimal::ZERO {
            return Err(KernelError::BadRequest(
                "order amount must be positive".into(),
            ));
        }
        let order_ref = digest("order_mock_", &request.idempotency_key);
        if let Ok(mut book) = self.orders.lock() {
            book.insert(order_ref.clone(), request.amount);
        }
        Ok(GatewayOrder {
            gateway: "mock",
            checkout_url: Some(format!("/fees?mockOrder={order_ref}")),
            order_ref,
            amount: request.amount,
            currency: request.currency,
            status: GatewayOrderStatus::Created,
        })
    }

    /// The deterministic outcome of an order this adapter created; `Pending` for unknown orders.
    async fn fetch_outcome(
        &self,
        _scope: &ExecutionScope,
        order_ref: &str,
    ) -> Result<GatewayPaymentOutcome, KernelError> {
        let amount = self
            .orders
            .lock()
            .ok()
            .and_then(|book| book.get(order_ref).copied());
        Ok(match amount {
            Some(a) => Self::outcome_for(order_ref, a),
            None => GatewayPaymentOutcome::Pending,
        })
    }

    fn verify_webhook(&self, body: &[u8], signature: &str) -> Result<GatewayWebhook, KernelError> {
        let mut mac = Hmac::<Sha256>::new_from_slice(self.secret.as_bytes())
            .map_err(|_| KernelError::Unauthenticated)?;
        mac.update(body);
        let expected = signature.trim();
        let bytes: Option<Vec<u8>> = (0..expected.len())
            .step_by(2)
            .map(|i| {
                expected
                    .get(i..i + 2)
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            })
            .collect();
        match bytes {
            Some(b) if expected.len() == 64 && mac.verify_slice(&b).is_ok() => {
                Self::parse_webhook(body)
            }
            _ => Err(KernelError::Unauthenticated),
        }
    }

    async fn refund(
        &self,
        _scope: &ExecutionScope,
        request: GatewayRefundRequest,
    ) -> Result<GatewayRefund, KernelError> {
        Ok(GatewayRefund {
            refund_ref: digest("rfnd_mock_", &request.idempotency_key),
            status: match paise(request.amount) {
                13 => GatewayRefundStatus::Failed,
                99 => GatewayRefundStatus::Pending,
                _ => GatewayRefundStatus::Processed,
            },
        })
    }
}

// INT: SEC-9D-003 — the "no gateway" adapter (`SUTRA_PAYMENT_GATEWAY=disabled`, the production
// default until a real gateway adapter exists). Every call is `Unavailable` (HTTP 503 problem
// `dependency-unavailable`), so student online checkout, recovery polling, gateway refunds and
// the webhook are switched off instead of capturing without money.

/// Adapter name of [`DisabledPaymentGateway`].
pub const DISABLED_GATEWAY: &str = "disabled";

/// Payment gateway switched off (feature off → 503).
#[derive(Clone, Copy, Debug, Default)]
pub struct DisabledPaymentGateway;

fn gateway_disabled() -> KernelError {
    KernelError::Unavailable("online payments are disabled (SUTRA_PAYMENT_GATEWAY=disabled)".into())
}

#[async_trait::async_trait]
impl PaymentGatewayPort for DisabledPaymentGateway {
    fn name(&self) -> &'static str {
        DISABLED_GATEWAY
    }

    async fn create_order(
        &self,
        _scope: &ExecutionScope,
        _request: GatewayOrderRequest,
    ) -> Result<GatewayOrder, KernelError> {
        Err(gateway_disabled())
    }

    async fn fetch_outcome(
        &self,
        _scope: &ExecutionScope,
        _order_ref: &str,
    ) -> Result<GatewayPaymentOutcome, KernelError> {
        Err(gateway_disabled())
    }

    fn verify_webhook(
        &self,
        _body: &[u8],
        _signature: &str,
    ) -> Result<GatewayWebhook, KernelError> {
        Err(gateway_disabled())
    }

    async fn refund(
        &self,
        _scope: &ExecutionScope,
        _request: GatewayRefundRequest,
    ) -> Result<GatewayRefund, KernelError> {
        Err(gateway_disabled())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use sutra_kernel::money::CurrencyCode;
    use sutra_kernel::scope::{InstitutionId, TenantId};
    use uuid::Uuid;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    #[tokio::test]
    async fn orders_are_idempotent_and_outcomes_deterministic() {
        let g = MockPaymentGateway::default();
        let scope = ExecutionScope::tenant(TenantId(Uuid::nil()));
        let req = GatewayOrderRequest {
            institution_id: InstitutionId(Uuid::nil()),
            amount: d("1500.00"),
            currency: CurrencyCode::INR,
            receipt_ref: "p1".into(),
            idempotency_key: "k1".into(),
        };
        let a = g.create_order(&scope, req.clone()).await.ok();
        let b = g.create_order(&scope, req).await.ok();
        assert!(a.is_some());
        assert_eq!(a, b);
        let order = a.map(|o| o.order_ref).unwrap_or_default();
        assert!(matches!(
            g.fetch_outcome(&scope, &order).await.ok(),
            Some(GatewayPaymentOutcome::Captured { .. })
        ));
        assert_eq!(
            g.fetch_outcome(&scope, "order_unknown").await.ok(),
            Some(GatewayPaymentOutcome::Pending)
        );
        assert!(matches!(
            MockPaymentGateway::outcome_for("o", d("10.13")),
            GatewayPaymentOutcome::Failed { .. }
        ));
        assert_eq!(
            MockPaymentGateway::outcome_for("o", d("10.99")),
            GatewayPaymentOutcome::Pending
        );
        assert!(matches!(
            MockPaymentGateway::outcome_for("o", d("10.00")),
            GatewayPaymentOutcome::Captured { .. }
        ));
    }

    #[test]
    fn webhook_signatures_are_verified() {
        let g = MockPaymentGateway::default();
        let body = br#"{"id":"evt_1","event":"payment.captured","orderId":"order_mock_x","paymentId":"pay_1","amount":"1500.00"}"#;
        let sig = g.sign(body);
        let w = g.verify_webhook(body, &sig).ok();
        assert_eq!(w.as_ref().map(|w| w.event_id.as_str()), Some("evt_1"));
        assert!(matches!(
            w.map(|w| w.outcome),
            Some(GatewayPaymentOutcome::Captured { .. })
        ));
        assert!(g.verify_webhook(body, &"0".repeat(64)).is_err());
        assert!(g.verify_webhook(b"{}", &sig).is_err(), "body tampered");
        let body =
            MockPaymentGateway::webhook_body("evt_2", "payment.failed", "order_x", d("10.13"));
        let w = g
            .verify_webhook(body.as_bytes(), &g.sign(body.as_bytes()))
            .ok();
        assert!(matches!(
            w.map(|w| w.outcome),
            Some(GatewayPaymentOutcome::Failed { .. })
        ));
    }

    // INT: SEC-9D-003.
    #[tokio::test]
    async fn disabled_gateway_refuses_everything() {
        let g = DisabledPaymentGateway;
        let scope = ExecutionScope::tenant(TenantId(Uuid::nil()));
        assert_eq!(g.name(), DISABLED_GATEWAY);
        let req = GatewayOrderRequest {
            institution_id: InstitutionId(Uuid::nil()),
            amount: d("1500.00"),
            currency: CurrencyCode::INR,
            receipt_ref: "r".into(),
            idempotency_key: "k".into(),
        };
        assert!(matches!(
            g.create_order(&scope, req).await,
            Err(KernelError::Unavailable(_))
        ));
        assert!(matches!(
            g.fetch_outcome(&scope, "order_x").await,
            Err(KernelError::Unavailable(_))
        ));
        let body =
            MockPaymentGateway::webhook_body("evt", "payment.captured", "order_x", d("1.00"));
        let sig = MockPaymentGateway::default().sign(body.as_bytes());
        assert!(matches!(
            g.verify_webhook(body.as_bytes(), &sig),
            Err(KernelError::Unavailable(_))
        ));
    }
}
