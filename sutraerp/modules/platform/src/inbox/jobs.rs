//! Retention sweepers of the Batch 1D tables (bounded, advisory-locked like every sweeper).
//!
//! * [`NotificationPurge`] — notifications older than the retention window that are read by
//!   their subject recipient, plus every notification older than twice the window (role
//!   addressed ones have no single reader). Receipts and deliveries cascade.
//! * [`NoticePurge`] — notices archived or expired longer ago than the retention window.
//! * [`TodoPurge`] — completed / dismissed to-dos closed longer ago than the window.
//! * [`EventReceiptPurge`] — consumer receipts older than the look-back window (+1 day), and
//!   expired replay-guard entries.

use sqlx::PgConnection;
use std::time::Duration;
use sutra_persistence::jobs::SweepJob;

fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

fn limit(n: u32) -> i64 {
    i64::from(n.max(1))
}

#[derive(Clone, Debug)]
pub struct NotificationPurge {
    pub interval: Duration,
    pub retention: Duration,
    pub batch_limit: u32,
}

#[async_trait::async_trait]
impl SweepJob for NotificationPurge {
    fn name(&self) -> &'static str {
        "notification-purge"
    }
    fn interval(&self) -> Duration {
        self.interval
    }
    async fn run(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        Ok(sqlx::query(
            "DELETE FROM platform.notification WHERE id = ANY(ARRAY( \
               SELECT n.id FROM platform.notification n \
               WHERE n.created_at < now() - make_interval(secs => $1 * 2) \
                  OR (n.created_at < now() - make_interval(secs => $1) \
                      AND n.recipient_subject IS NOT NULL \
                      AND EXISTS (SELECT 1 FROM platform.notification_read r \
                                  WHERE r.notification_id = n.id AND r.subject = n.recipient_subject)) \
               LIMIT $2))",
        )
        .bind(secs(self.retention))
        .bind(limit(self.batch_limit))
        .execute(conn)
        .await?
        .rows_affected())
    }
}

#[derive(Clone, Debug)]
pub struct NoticePurge {
    pub interval: Duration,
    pub retention: Duration,
    pub batch_limit: u32,
}

#[async_trait::async_trait]
impl SweepJob for NoticePurge {
    fn name(&self) -> &'static str {
        "notice-purge"
    }
    fn interval(&self) -> Duration {
        self.interval
    }
    async fn run(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        Ok(sqlx::query(
            "DELETE FROM platform.notice WHERE id = ANY(ARRAY( \
               SELECT id FROM platform.notice \
               WHERE archived_at < now() - make_interval(secs => $1) \
                  OR expires_at < now() - make_interval(secs => $1) \
               LIMIT $2))",
        )
        .bind(secs(self.retention))
        .bind(limit(self.batch_limit))
        .execute(conn)
        .await?
        .rows_affected())
    }
}

#[derive(Clone, Debug)]
pub struct TodoPurge {
    pub interval: Duration,
    pub retention: Duration,
    pub batch_limit: u32,
}

#[async_trait::async_trait]
impl SweepJob for TodoPurge {
    fn name(&self) -> &'static str {
        "todo-purge"
    }
    fn interval(&self) -> Duration {
        self.interval
    }
    async fn run(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        Ok(sqlx::query(
            "DELETE FROM platform.todo WHERE id = ANY(ARRAY( \
               SELECT id FROM platform.todo \
               WHERE status <> 'open' AND updated_at < now() - make_interval(secs => $1) \
               LIMIT $2))",
        )
        .bind(secs(self.retention))
        .bind(limit(self.batch_limit))
        .execute(conn)
        .await?
        .rows_affected())
    }
}

#[derive(Clone, Debug)]
pub struct EventReceiptPurge {
    pub interval: Duration,
    /// Consumer look-back window; receipts are kept one day longer.
    pub lookback: Duration,
    pub batch_limit: u32,
}

#[async_trait::async_trait]
impl SweepJob for EventReceiptPurge {
    fn name(&self) -> &'static str {
        "event-receipt-purge"
    }
    fn interval(&self) -> Duration {
        self.interval
    }
    async fn run(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        let receipts = sqlx::query(
            "DELETE FROM platform.event_receipt WHERE ctid = ANY(ARRAY( \
               SELECT ctid FROM platform.event_receipt \
               WHERE received_at < now() - make_interval(secs => $1 + 86400) LIMIT $2))",
        )
        .bind(secs(self.lookback))
        .bind(limit(self.batch_limit))
        .execute(&mut *conn)
        .await?
        .rows_affected();
        let guards = sqlx::query(
            "DELETE FROM platform.replay_guard WHERE key_hash = ANY(ARRAY( \
               SELECT key_hash FROM platform.replay_guard WHERE expires_at < now() LIMIT $1))",
        )
        .bind(limit(self.batch_limit))
        .execute(&mut *conn)
        .await?
        .rows_affected();
        Ok(receipts + guards)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_names_are_stable() {
        let d = Duration::from_secs(1);
        let names = [
            NotificationPurge {
                interval: d,
                retention: d,
                batch_limit: 1,
            }
            .name(),
            NoticePurge {
                interval: d,
                retention: d,
                batch_limit: 1,
            }
            .name(),
            TodoPurge {
                interval: d,
                retention: d,
                batch_limit: 1,
            }
            .name(),
            EventReceiptPurge {
                interval: d,
                lookback: d,
                batch_limit: 1,
            }
            .name(),
        ];
        assert_eq!(
            names,
            [
                "notification-purge",
                "notice-purge",
                "todo-purge",
                "event-receipt-purge"
            ]
        );
    }
}
