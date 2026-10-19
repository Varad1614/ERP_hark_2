//! Delivery-channel port for notifications.
//!
//! The in-app channel *is* the `platform.notification` table. Out-of-band channels are queued in
//! `platform.notification_delivery` in the transaction that creates the notification and sent by
//! [`NotificationDelivery`] — an advisory-locked job (one replica at a time) that claims due rows
//! with `FOR UPDATE SKIP LOCKED`, hands them to the [`MailSender`] port (SMTP adapter in
//! `sutra-infra`, Mailpit in development) and records the outcome: `sent`, or a retry with
//! exponential backoff, or `failed` after `max_attempts` / a permanent error.

use sqlx::{PgConnection, Row};
use std::sync::Arc;
use std::time::Duration;
use sutra_kernel::mail::{MailError, MailMessage, MailSender};
use sutra_persistence::jobs::SweepJob;

/// Backoff before retry `attempt` (1-based): 30 s · 2^(attempt-1), capped at one hour.
pub fn retry_delay(attempt: i32) -> Duration {
    let exp = u32::try_from(attempt.saturating_sub(1)).unwrap_or(0).min(7);
    Duration::from_secs((30u64 << exp).min(3600))
}

/// Plain-text e-mail body of a notification.
pub fn render(title: &str, body: &str, link: Option<&str>, public_url: &str) -> String {
    let mut text = String::new();
    text.push_str(title);
    text.push_str("\n\n");
    if !body.is_empty() {
        text.push_str(body);
        text.push_str("\n\n");
    }
    if let Some(l) = link {
        text.push_str(&format!(
            "Open in SutraERP: {}{l}\n\n",
            public_url.trim_end_matches('/')
        ));
    }
    text.push_str("You receive this because e-mail is enabled for this kind of notification in your SutraERP profile.\n");
    text
}

/// E-mail delivery job.
#[derive(Clone)]
pub struct NotificationDelivery {
    pub interval: Duration,
    pub mailer: Arc<dyn MailSender>,
    /// Browser-facing base URL used for links in e-mails.
    pub public_url: String,
    pub batch_size: u32,
    pub max_attempts: i32,
    pub send_timeout: Duration,
}

#[async_trait::async_trait]
impl SweepJob for NotificationDelivery {
    fn name(&self) -> &'static str {
        "notification-delivery"
    }
    fn interval(&self) -> Duration {
        self.interval
    }
    async fn run(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT d.id, d.address, d.attempts, n.title, n.body, n.link \
             FROM platform.notification_delivery d \
             JOIN platform.notification n ON n.id = d.notification_id \
             WHERE d.status = 'pending' AND d.next_attempt_at <= now() \
             ORDER BY d.next_attempt_at LIMIT $1 FOR UPDATE OF d SKIP LOCKED",
        )
        .bind(i64::from(self.batch_size.max(1)))
        .fetch_all(&mut *conn)
        .await?;
        let mut sent = 0;
        for r in rows {
            let id: uuid::Uuid = r.try_get("id")?;
            let attempts: i32 = r.try_get::<i32, _>("attempts")? + 1;
            let title: String = r.try_get("title")?;
            let body: String = r.try_get("body")?;
            let link: Option<String> = r.try_get("link")?;
            let msg = MailMessage {
                to: r.try_get("address")?,
                subject: format!("[SutraERP] {title}"),
                text: render(&title, &body, link.as_deref(), &self.public_url),
            };
            let outcome =
                match tokio::time::timeout(self.send_timeout, self.mailer.send(&msg)).await {
                    Ok(r) => r,
                    Err(_) => Err(MailError::Transient("send timed out".into())),
                };
            match outcome {
                Ok(()) => {
                    sqlx::query(
                        "UPDATE platform.notification_delivery SET status = 'sent', sent_at = now(), \
                           attempts = $2, last_error = NULL WHERE id = $1",
                    )
                    .bind(id)
                    .bind(attempts)
                    .execute(&mut *conn)
                    .await?;
                    sent += 1;
                }
                Err(e) => {
                    let permanent =
                        matches!(e, MailError::Permanent(_)) || attempts >= self.max_attempts;
                    tracing::warn!(delivery = %id, attempts, permanent, error = %e, "notification e-mail failed");
                    sqlx::query(
                        "UPDATE platform.notification_delivery SET attempts = $2, last_error = $3, \
                           status = CASE WHEN $4 THEN 'failed' ELSE 'pending' END, \
                           next_attempt_at = now() + make_interval(secs => $5) WHERE id = $1",
                    )
                    .bind(id)
                    .bind(attempts)
                    .bind(e.to_string())
                    .bind(permanent)
                    .bind(retry_delay(attempts).as_secs_f64())
                    .execute(&mut *conn)
                    .await?;
                }
            }
        }
        Ok(sent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_and_rendering() {
        assert_eq!(retry_delay(1), Duration::from_secs(30));
        assert_eq!(retry_delay(2), Duration::from_secs(60));
        assert_eq!(retry_delay(20), Duration::from_secs(3600));
        let t = render("New notice", "", Some("/notices"), "http://localhost:3000/");
        assert!(t.contains("http://localhost:3000/notices"));
        assert!(!t.contains("\n\n\n"));
    }
}
