//! Outbound e-mail port (notification e-mail channel, Batch 1D).
//!
//! Modules hand a [`MailMessage`] to a [`MailSender`]; the SMTP adapter lives in `sutra-infra`
//! (Mailpit in development). [`NoopMailSender`] is used when the e-mail channel is off and
//! [`InMemoryMailSender`] records messages for tests.

use crate::error::KernelError;
use std::fmt;
use std::sync::Mutex;

/// A plain-text e-mail. `Debug` hides the body (it may carry personal data).
#[derive(Clone, PartialEq, Eq)]
pub struct MailMessage {
    pub to: String,
    pub subject: String,
    pub text: String,
}

impl fmt::Debug for MailMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MailMessage")
            .field("to", &self.to)
            .field("subject", &self.subject)
            .finish_non_exhaustive()
    }
}

/// Why a message could not be sent: transient failures are retried, permanent ones are not.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MailError {
    #[error("transient mail failure: {0}")]
    Transient(String),
    #[error("permanent mail failure: {0}")]
    Permanent(String),
}

impl From<MailError> for KernelError {
    fn from(e: MailError) -> Self {
        KernelError::Unavailable(e.to_string())
    }
}

#[async_trait::async_trait]
pub trait MailSender: Send + Sync + 'static {
    /// Adapter name for logs (`smtp`, `noop`, `memory`).
    fn backend(&self) -> &'static str;
    async fn send(&self, message: &MailMessage) -> Result<(), MailError>;
}

/// E-mail channel disabled: every send is a permanent "not configured" failure, so nothing is
/// queued for retry.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopMailSender;

#[async_trait::async_trait]
impl MailSender for NoopMailSender {
    fn backend(&self) -> &'static str {
        "noop"
    }
    async fn send(&self, _: &MailMessage) -> Result<(), MailError> {
        Err(MailError::Permanent(
            "e-mail channel is not configured".into(),
        ))
    }
}

/// Test adapter that records every message.
#[derive(Debug, Default)]
pub struct InMemoryMailSender {
    sent: Mutex<Vec<MailMessage>>,
}

impl InMemoryMailSender {
    pub fn sent(&self) -> Vec<MailMessage> {
        self.sent.lock().map(|v| v.clone()).unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl MailSender for InMemoryMailSender {
    fn backend(&self) -> &'static str {
        "memory"
    }
    async fn send(&self, message: &MailMessage) -> Result<(), MailError> {
        self.sent
            .lock()
            .map_err(|_| MailError::Transient("mailbox poisoned".into()))?
            .push(message.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn adapters_behave_and_debug_hides_the_body() {
        let m = MailMessage {
            to: "a@b.example".into(),
            subject: "Hello".into(),
            text: "secret body".into(),
        };
        assert!(!format!("{m:?}").contains("secret body"));
        assert!(matches!(
            NoopMailSender.send(&m).await,
            Err(MailError::Permanent(_))
        ));
        let mem = InMemoryMailSender::default();
        mem.send(&m).await.unwrap_or_default();
        assert_eq!(mem.sent(), vec![m]);
        assert_eq!(mem.backend(), "memory");
    }
}
