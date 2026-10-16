//! Part 2 S5-monitor: operational signals for monitoring (docs/readiness/MONITORING_AND_SUPPORT.md).
//!
//! Shared crates and modules emit **bounded** signals here — a security event or the outcome of
//! one call to an external system — as `tracing` events on the `sutra::metrics` target. The API's
//! `EventMetricsLayer` (apps/api/src/metrics.rs) turns them into the Prometheus counters
//! `sutra_security_events_total{event}` and `sutra_integration_calls_total{integration, outcome}`,
//! so no crate needs a metrics dependency and no label can carry an id, a login or a tenant:
//! every label value is one of the `as_str` constants below.

/// Security-relevant events counted for alerting (spikes, not individual audit evidence — the
/// audit trail stays the record of who did what).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecurityEvent {
    /// A sign-in was rejected (wrong credentials, disabled account, …).
    LoginFailed,
    /// A sign-in was refused because the login is temporarily locked (throttle).
    LoginThrottled,
    /// Repeated failures locked a login.
    AccountLocked,
    /// A second-factor code / assertion was rejected.
    MfaFailed,
    /// Repeated second-factor failures locked the factor.
    MfaLocked,
    /// Roles of an identity were granted / revoked (applied).
    PrivilegeChanged,
    /// A request named a tenant / institution outside its session scope (403).
    CrossTenantDenied,
    /// A payment-gateway webhook arrived without a valid signature (401).
    WebhookSignatureFailed,
    /// A checkout callback signature did not verify (422).
    CheckoutSignatureFailed,
}

impl SecurityEvent {
    /// Every event (label values primed at zero so the series exist before the first event).
    pub const ALL: [Self; 9] = [
        Self::LoginFailed,
        Self::LoginThrottled,
        Self::AccountLocked,
        Self::MfaFailed,
        Self::MfaLocked,
        Self::PrivilegeChanged,
        Self::CrossTenantDenied,
        Self::WebhookSignatureFailed,
        Self::CheckoutSignatureFailed,
    ];

    /// The `event` label value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LoginFailed => "login_failed",
            Self::LoginThrottled => "login_throttled",
            Self::AccountLocked => "account_locked",
            Self::MfaFailed => "mfa_failed",
            Self::MfaLocked => "mfa_locked",
            Self::PrivilegeChanged => "privilege_changed",
            Self::CrossTenantDenied => "cross_tenant_denied",
            Self::WebhookSignatureFailed => "webhook_signature_failed",
            Self::CheckoutSignatureFailed => "checkout_signature_failed",
        }
    }

    /// Parses a label value (unknown values are rejected, never exported).
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.as_str() == s)
    }
}

/// External systems whose calls are counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Integration {
    Razorpay,
    S3,
    KeycloakAdmin,
    Smtp,
}

impl Integration {
    pub const ALL: [Self; 4] = [Self::Razorpay, Self::S3, Self::KeycloakAdmin, Self::Smtp];

    /// The `integration` label value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Razorpay => "razorpay",
            Self::S3 => "s3",
            Self::KeycloakAdmin => "keycloak_admin",
            Self::Smtp => "smtp",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.as_str() == s)
    }
}

/// Outcome of one call (one HTTP request / one SMTP submission).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallOutcome {
    /// Answered, and not with a server-side / credential error.
    Ok,
    /// Transport failure, 5xx, or the provider refused our credentials (401/403).
    Error,
    /// The request timed out.
    Timeout,
}

impl CallOutcome {
    pub const ALL: [Self; 3] = [Self::Ok, Self::Error, Self::Timeout];

    /// The `outcome` label value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Timeout => "timeout",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.as_str() == s)
    }

    /// Classifies an HTTP status answered by a provider: 5xx and 401/403 (our credentials or
    /// configuration are wrong) are errors; anything else (2xx, 3xx, a 4xx about the request
    /// itself such as 404 / 409 / 422) means the provider is up and answering.
    pub const fn from_status(status: u16) -> Self {
        if status >= 500 || status == 401 || status == 403 {
            Self::Error
        } else {
            Self::Ok
        }
    }
}

/// Emits one security event.
pub fn security(event: SecurityEvent) {
    tracing::info!(
        target: "sutra::metrics",
        security_event = event.as_str(),
        "security signal"
    );
}

/// Emits the outcome of one external call.
pub fn integration(integration: Integration, outcome: CallOutcome) {
    tracing::info!(
        target: "sutra::metrics",
        integration = integration.as_str(),
        integration_outcome = outcome.as_str(),
        "integration call"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_values_round_trip_and_are_snake_case() {
        for e in SecurityEvent::ALL {
            assert_eq!(SecurityEvent::parse(e.as_str()), Some(e));
            assert!(e
                .as_str()
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'_'));
        }
        for i in Integration::ALL {
            assert_eq!(Integration::parse(i.as_str()), Some(i));
        }
        for o in CallOutcome::ALL {
            assert_eq!(CallOutcome::parse(o.as_str()), Some(o));
        }
        assert_eq!(SecurityEvent::parse("tenant-123"), None);
        assert_eq!(Integration::parse("unknown"), None);
    }

    #[test]
    fn http_status_classification() {
        assert_eq!(CallOutcome::from_status(200), CallOutcome::Ok);
        assert_eq!(CallOutcome::from_status(404), CallOutcome::Ok);
        assert_eq!(CallOutcome::from_status(422), CallOutcome::Ok);
        assert_eq!(CallOutcome::from_status(401), CallOutcome::Error);
        assert_eq!(CallOutcome::from_status(403), CallOutcome::Error);
        assert_eq!(CallOutcome::from_status(500), CallOutcome::Error);
        assert_eq!(CallOutcome::from_status(503), CallOutcome::Error);
    }
}
