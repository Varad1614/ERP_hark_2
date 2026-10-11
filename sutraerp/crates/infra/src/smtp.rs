//! SMTP [`MailSender`] adapter (lettre, rustls) for the notification e-mail channel.
//!
//! * `SUTRA_SMTP_TLS=none` — plain SMTP (Mailpit / a local relay on a trusted network only),
//! * `starttls` — STARTTLS required (submission, port 587),
//! * `tls` — implicit TLS (submissions, port 465).
//!
//! Credentials are optional. SMTP 4xx replies and connection failures are transient (the
//! delivery job retries with backoff); 5xx replies and invalid addresses are permanent.

use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use std::fmt;
use std::time::Duration;
use sutra_kernel::mail::{MailError, MailMessage, MailSender};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmtpTls {
    None,
    StartTls,
    Tls,
}

#[derive(Clone)]
pub struct SmtpSettings {
    pub host: String,
    pub port: u16,
    pub tls: SmtpTls,
    pub username: Option<String>,
    pub password: Option<String>,
    /// `Display Name <address>` or a bare address.
    pub from: String,
    pub timeout: Duration,
}

impl fmt::Debug for SmtpSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SmtpSettings")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("from", &self.from)
            .finish()
    }
}

pub struct SmtpMailSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl SmtpMailSender {
    pub fn new(s: &SmtpSettings) -> Result<Self, String> {
        let from: Mailbox = s
            .from
            .parse()
            .map_err(|e| format!("SUTRA_MAIL_FROM `{}`: {e}", s.from))?;
        let mut b = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(s.host.as_str())
            .port(s.port)
            .timeout(Some(s.timeout));
        b = match s.tls {
            SmtpTls::None => b.tls(Tls::None),
            SmtpTls::StartTls | SmtpTls::Tls => {
                let params = TlsParameters::new(s.host.clone())
                    .map_err(|e| format!("SMTP TLS parameters: {e}"))?;
                if s.tls == SmtpTls::StartTls {
                    b.tls(Tls::Required(params))
                } else {
                    b.tls(Tls::Wrapper(params))
                }
            }
        };
        if let (Some(u), Some(p)) = (&s.username, &s.password) {
            b = b.credentials(Credentials::new(u.clone(), p.clone()));
        }
        Ok(Self {
            transport: b.build(),
            from,
        })
    }
}

#[async_trait::async_trait]
impl MailSender for SmtpMailSender {
    fn backend(&self) -> &'static str {
        "smtp"
    }

    async fn send(&self, m: &MailMessage) -> Result<(), MailError> {
        let to: Mailbox =
            m.to.parse()
                .map_err(|e| MailError::Permanent(format!("recipient address: {e}")))?;
        let email = Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject(m.subject.clone())
            .body(m.text.clone())
            .map_err(|e| MailError::Permanent(format!("message: {e}")))?;
        self.transport.send(email).await.map(|_| ()).map_err(|e| {
            if e.is_permanent() {
                MailError::Permanent(e.to_string())
            } else {
                MailError::Transient(e.to_string())
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(from: &str) -> SmtpSettings {
        SmtpSettings {
            host: "localhost".into(),
            port: 1025,
            tls: SmtpTls::None,
            username: Some("u".into()),
            password: Some("secret-pw".into()),
            from: from.into(),
            timeout: Duration::from_millis(500),
        }
    }

    #[test]
    fn builds_and_redacts() {
        assert!(SmtpMailSender::new(&settings("SutraERP <no-reply@sutra.dev>")).is_ok());
        assert!(SmtpMailSender::new(&settings("not an address")).is_err());
        assert!(!format!("{:?}", settings("a@b.c")).contains("secret-pw"));
    }

    #[tokio::test]
    async fn unreachable_server_is_transient_and_bad_recipient_permanent() {
        let mut s = settings("a@b.dev");
        s.port = 1; // nothing listens here
        let m = SmtpMailSender::new(&s).unwrap_or_else(|e| panic!("{e}"));
        let msg = MailMessage {
            to: "x@y.dev".into(),
            subject: "s".into(),
            text: "t".into(),
        };
        assert!(matches!(m.send(&msg).await, Err(MailError::Transient(_))));
        let bad = MailMessage {
            to: "nope".into(),
            ..msg
        };
        assert!(matches!(m.send(&bad).await, Err(MailError::Permanent(_))));
    }
}
