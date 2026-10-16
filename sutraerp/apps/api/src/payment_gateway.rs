//! Part 2 · Stage 4 (S4-pay): the Razorpay adapter behind
//! [`PaymentGatewayPort`] — the composition root wraps `sutra_infra::razorpay::RazorpayClient`
//! (plain JSON + integer paise) into the finance port (decimal rupees, INR only).
//!
//! * `create_order` → `POST /v1/orders` with `receipt = sutra_<sha256(idempotency key)[..32]>`
//!   (≤ 40 chars; Razorpay treats the receipt as unique, so a retried create returns the same
//!   order). The amount and currency Razorpay answers are checked against the request.
//! * `fetch_outcome` → `GET /v1/orders/{id}/payments`: captured → `Captured`, an attempt in flight
//!   → `Pending`, only failed attempts → `Failed`; a timeout is an error (`Unavailable`), never a
//!   guessed outcome (L3-F-010).
//! * `verify_webhook` → HMAC-SHA256 of the raw body with the webhook secret (`X-Razorpay-Signature`,
//!   constant time); event id = `"<event>:<entity id>"` from the signed body.
//! * `verify_checkout` → HMAC-SHA256(`order_id|payment_id`, key secret).
//! * `refund` → `POST /v1/payments/{id}/refund` with `X-Refund-Idempotency` / `receipt` derived
//!   from the refund attempt key.
//!
//! There is no hosted checkout URL: the payer completes the payment in Razorpay Checkout
//! (`checkout.js`) with the order id and the public key id, so `checkout_url` is `None`.

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::json;
use sutra_contracts::finance::ports::{
    GatewayOrder, GatewayOrderRequest, GatewayOrderStatus, GatewayPaymentOutcome, GatewayRefund,
    GatewayRefundRequest, GatewayRefundStatus, GatewayWebhook, PaymentGatewayPort,
};
use sutra_infra::razorpay::{
    outcome_of, receipt_for, refund_key_for, RazorpayClient, RazorpaySettings, RzOutcome, RzPayment,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::money::CurrencyCode;
use sutra_kernel::scope::ExecutionScope;

/// Adapter name stored on payments (`finance.payment.gateway`).
pub const RAZORPAY: &str = "razorpay";

/// Decimal rupees → integer paise (positive, at most two decimals).
pub fn to_paise(amount: Decimal) -> Result<i64, KernelError> {
    let paise = amount * Decimal::from(100);
    if amount <= Decimal::ZERO || !paise.fract().is_zero() {
        return Err(KernelError::BadRequest(format!(
            "gateway amounts must be positive with at most two decimals (got {amount})"
        )));
    }
    paise
        .trunc()
        .to_i64()
        .ok_or_else(|| KernelError::BadRequest(format!("amount out of range: {amount}")))
}

/// Integer paise → decimal rupees.
pub fn from_paise(paise: i64) -> Decimal {
    Decimal::new(paise, 2)
}

fn require_inr(currency: &str) -> Result<(), KernelError> {
    if currency.eq_ignore_ascii_case("INR") {
        Ok(())
    } else {
        Err(KernelError::Conflict(format!(
            "razorpay answered in {currency}; only INR is supported"
        )))
    }
}

fn captured(p: &RzPayment) -> Result<GatewayPaymentOutcome, KernelError> {
    require_inr(&p.currency)?;
    Ok(GatewayPaymentOutcome::Captured {
        payment_ref: p.id.clone(),
        amount: from_paise(p.amount),
    })
}

fn failed(p: &RzPayment) -> GatewayPaymentOutcome {
    GatewayPaymentOutcome::Failed {
        code: p
            .error_code
            .clone()
            .unwrap_or_else(|| "PAYMENT_FAILED".into()),
        reason: p
            .error_description
            .clone()
            .unwrap_or_else(|| "payment failed at the gateway".into()),
    }
}

/// The Razorpay payment gateway.
#[derive(Clone, Debug)]
pub struct RazorpayGateway {
    client: RazorpayClient,
}

impl RazorpayGateway {
    pub fn new(settings: RazorpaySettings) -> Result<Self, String> {
        Ok(Self {
            client: RazorpayClient::new(settings)?,
        })
    }

    pub fn client(&self) -> &RazorpayClient {
        &self.client
    }
}

#[async_trait::async_trait]
impl PaymentGatewayPort for RazorpayGateway {
    fn name(&self) -> &'static str {
        RAZORPAY
    }

    async fn create_order(
        &self,
        _scope: &ExecutionScope,
        request: GatewayOrderRequest,
    ) -> Result<GatewayOrder, KernelError> {
        if request.currency != CurrencyCode::INR {
            return Err(KernelError::BadRequest(
                "only INR orders are supported".into(),
            ));
        }
        let paise = to_paise(request.amount)?;
        let receipt = receipt_for(&request.idempotency_key);
        let order = self
            .client
            .create_order(
                paise,
                "INR",
                &receipt,
                json!({"source": "sutraerp", "institution": request.institution_id.as_uuid().to_string()}),
            )
            .await?;
        if order.amount != paise {
            return Err(KernelError::Conflict(format!(
                "razorpay order {} is for {} paise, not {paise} (receipt reused?)",
                order.id, order.amount
            )));
        }
        require_inr(&order.currency)?;
        Ok(GatewayOrder {
            gateway: RAZORPAY,
            order_ref: order.id,
            amount: request.amount,
            currency: request.currency,
            status: if order.status == "paid" {
                GatewayOrderStatus::Paid
            } else {
                GatewayOrderStatus::Created
            },
            checkout_url: None,
        })
    }

    async fn fetch_outcome(
        &self,
        _scope: &ExecutionScope,
        order_ref: &str,
    ) -> Result<GatewayPaymentOutcome, KernelError> {
        let payments = self.client.order_payments(order_ref).await?;
        match outcome_of(&payments) {
            RzOutcome::Captured(p) => captured(&p),
            RzOutcome::Failed(p) => Ok(failed(&p)),
            RzOutcome::Pending => Ok(GatewayPaymentOutcome::Pending),
        }
    }

    fn verify_webhook(&self, body: &[u8], signature: &str) -> Result<GatewayWebhook, KernelError> {
        let w = self.client.verify_webhook(body, signature)?;
        let outcome = match (w.event.as_str(), w.payment.as_ref()) {
            ("payment.captured" | "order.paid", Some(p))
                if p.captured || p.status == "captured" =>
            {
                captured(p)?
            }
            ("payment.failed", Some(p)) => failed(p),
            _ => GatewayPaymentOutcome::Pending,
        };
        Ok(GatewayWebhook {
            event_id: w.event_id,
            event: w.event,
            order_ref: w.order_id,
            outcome,
        })
    }

    async fn refund(
        &self,
        _scope: &ExecutionScope,
        request: GatewayRefundRequest,
    ) -> Result<GatewayRefund, KernelError> {
        if request.currency != CurrencyCode::INR {
            return Err(KernelError::BadRequest(
                "only INR refunds are supported".into(),
            ));
        }
        let paise = to_paise(request.amount)?;
        let key = refund_key_for(&request.idempotency_key);
        let r = self
            .client
            .refund(&request.payment_ref, paise, &key)
            .await?;
        Ok(GatewayRefund {
            refund_ref: r.id,
            status: match r.status.as_str() {
                "processed" => GatewayRefundStatus::Processed,
                "failed" => GatewayRefundStatus::Failed,
                _ => GatewayRefundStatus::Pending,
            },
        })
    }

    fn verify_checkout(
        &self,
        order_ref: &str,
        payment_ref: &str,
        signature: &str,
    ) -> Result<(), KernelError> {
        if self
            .client
            .verify_checkout(order_ref, payment_ref, signature)
        {
            Ok(())
        } else {
            Err(KernelError::Unauthenticated)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use std::time::Duration;
    use sutra_infra::razorpay::stub::{Attempt, Fault, RazorpayStub};
    use sutra_kernel::scope::{InstitutionId, TenantId};
    use uuid::Uuid;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn scope() -> ExecutionScope {
        ExecutionScope::tenant(TenantId(Uuid::nil()))
    }

    fn order_req(amount: &str, key: &str) -> GatewayOrderRequest {
        GatewayOrderRequest {
            institution_id: InstitutionId(Uuid::nil()),
            amount: d(amount),
            currency: CurrencyCode::INR,
            receipt_ref: key.into(),
            idempotency_key: key.into(),
        }
    }

    #[test]
    fn paise_conversion_is_exact() {
        assert_eq!(to_paise(d("1500.50")).unwrap(), 150_050);
        assert_eq!(to_paise(d("1.00")).unwrap(), 100);
        assert_eq!(to_paise(d("10.100")).unwrap(), 1010);
        assert!(to_paise(d("1.005")).is_err());
        assert!(to_paise(d("0")).is_err());
        assert!(to_paise(d("-5")).is_err());
        assert_eq!(from_paise(150_050), d("1500.50"));
    }

    #[tokio::test]
    async fn the_adapter_speaks_the_port_over_the_stub() {
        let stub = RazorpayStub::start().await.unwrap();
        let g = RazorpayGateway::new(stub.settings(Duration::from_secs(2), 1)).unwrap();
        assert_eq!(g.name(), "razorpay");
        let o = g
            .create_order(&scope(), order_req("2500.00", "t:a:k1"))
            .await
            .unwrap();
        assert_eq!(
            (o.gateway, o.amount, o.status),
            ("razorpay", d("2500.00"), GatewayOrderStatus::Created)
        );
        assert!(o.checkout_url.is_none());
        assert_eq!(
            g.create_order(&scope(), order_req("2500.00", "t:a:k1"))
                .await
                .unwrap()
                .order_ref,
            o.order_ref,
            "same idempotency key → same order"
        );
        assert!(matches!(
            g.create_order(&scope(), order_req("1.005", "t:a:k2")).await,
            Err(KernelError::BadRequest(_))
        ));
        assert_eq!(
            g.fetch_outcome(&scope(), &o.order_ref).await.unwrap(),
            GatewayPaymentOutcome::Pending
        );
        let failed = stub.pay(&o.order_ref, Attempt::Failed);
        assert!(matches!(
            g.fetch_outcome(&scope(), &o.order_ref).await.unwrap(),
            GatewayPaymentOutcome::Failed { code, .. } if code == "BAD_REQUEST_ERROR"
        ));
        let p = stub.pay(&o.order_ref, Attempt::Captured);
        let pid = p["id"].as_str().unwrap().to_owned();
        assert_eq!(
            g.fetch_outcome(&scope(), &o.order_ref).await.unwrap(),
            GatewayPaymentOutcome::Captured {
                payment_ref: pid.clone(),
                amount: d("2500.00")
            }
        );
        // Webhooks: captured / failed, tampered → 401, foreign secret → 401.
        let (body, sig) = stub.webhook("payment.captured", &p);
        let w = g.verify_webhook(body.as_bytes(), &sig).unwrap();
        assert_eq!(w.order_ref, o.order_ref);
        assert_eq!(w.event_id, format!("payment.captured:{pid}"));
        assert!(
            matches!(w.outcome, GatewayPaymentOutcome::Captured { amount, .. } if amount == d("2500.00"))
        );
        let (fbody, fsig) = stub.webhook("payment.failed", &failed);
        assert!(matches!(
            g.verify_webhook(fbody.as_bytes(), &fsig).unwrap().outcome,
            GatewayPaymentOutcome::Failed { .. }
        ));
        assert!(matches!(
            g.verify_webhook(body.replace("captured", "failed").as_bytes(), &sig),
            Err(KernelError::Unauthenticated)
        ));
        // Checkout callback signature.
        assert!(g
            .verify_checkout(
                &o.order_ref,
                &pid,
                &stub.checkout_signature(&o.order_ref, &pid)
            )
            .is_ok());
        assert!(g
            .verify_checkout(&o.order_ref, &pid, &"0".repeat(64))
            .is_err());
        // Refund (idempotent on the key, retried through a lost answer).
        stub.inject("POST /v1/payments/refund", Fault::LoseAnswer);
        let req = GatewayRefundRequest {
            payment_ref: pid.clone(),
            amount: d("1000.00"),
            currency: CurrencyCode::INR,
            idempotency_key: "refund:x:1".into(),
        };
        let r = g.refund(&scope(), req.clone()).await.unwrap();
        assert_eq!(r.status, GatewayRefundStatus::Processed);
        assert_eq!(
            g.refund(&scope(), req).await.unwrap().refund_ref,
            r.refund_ref
        );
    }

    #[tokio::test]
    async fn an_unreachable_gateway_is_unavailable_not_pending() {
        let stub = RazorpayStub::start().await.unwrap();
        let g = RazorpayGateway::new(stub.settings(Duration::from_millis(150), 1)).unwrap();
        let o = g
            .create_order(&scope(), order_req("100.00", "t:a:u"))
            .await
            .unwrap();
        stub.inject("GET /v1/orders/payments", Fault::Status(502));
        stub.inject(
            "GET /v1/orders/payments",
            Fault::Delay(Duration::from_millis(500)),
        );
        assert!(matches!(
            g.fetch_outcome(&scope(), &o.order_ref).await,
            Err(KernelError::Unavailable(_))
        ));
    }
}
