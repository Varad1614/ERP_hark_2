//! Platform-owned sweeper jobs (run by `sutra_persistence::jobs::Scheduler`). They purge dead
//! D01 records from this module's own tables; each run is bounded by `batch_limit` rows.

use sqlx::PgConnection;
use std::time::Duration;
use sutra_persistence::jobs::SweepJob;

fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

fn limit(n: u32) -> i64 {
    i64::from(n.max(1))
}

/// Deletes sessions that are dead for longer than `grace`: absolutely expired, revoked, or idle
/// past `idle_timeout`. The grace period keeps recently ended sessions available for audit and
/// "why was I signed out" support before they disappear.
#[derive(Clone, Debug)]
pub struct SessionPurge {
    pub interval: Duration,
    pub grace: Duration,
    pub idle_timeout: Duration,
    pub batch_limit: u32,
}

#[async_trait::async_trait]
impl SweepJob for SessionPurge {
    fn name(&self) -> &'static str {
        "session-purge"
    }
    fn interval(&self) -> Duration {
        self.interval
    }
    async fn run(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        Ok(sqlx::query(
            "DELETE FROM platform.session WHERE ctid = ANY(ARRAY( \
               SELECT ctid FROM platform.session \
               WHERE expires_at < now() - make_interval(secs => $1) \
                  OR revoked_at < now() - make_interval(secs => $1) \
                  OR last_seen_at < now() - make_interval(secs => $1 + $2) \
               LIMIT $3))",
        )
        .bind(secs(self.grace))
        .bind(secs(self.idle_timeout))
        .bind(limit(self.batch_limit))
        .execute(conn)
        .await?
        .rows_affected())
    }
}

/// Deletes pre-auth records (OIDC authorization state, MFA challenges) that expired more than
/// `grace` ago. Consumed records are already gone: `take` deletes them atomically.
#[derive(Clone, Debug)]
pub struct PreAuthPurge {
    pub interval: Duration,
    pub grace: Duration,
    pub batch_limit: u32,
}

#[async_trait::async_trait]
impl SweepJob for PreAuthPurge {
    fn name(&self) -> &'static str {
        "pre-auth-purge"
    }
    fn interval(&self) -> Duration {
        self.interval
    }
    async fn run(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        Ok(sqlx::query(
            "DELETE FROM platform.pre_auth WHERE ctid = ANY(ARRAY( \
               SELECT ctid FROM platform.pre_auth \
               WHERE expires_at <= now() - make_interval(secs => $1) LIMIT $2))",
        )
        .bind(secs(self.grace))
        .bind(limit(self.batch_limit))
        .execute(conn)
        .await?
        .rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_names_are_stable() {
        let s = SessionPurge {
            interval: Duration::from_secs(1),
            grace: Duration::ZERO,
            idle_timeout: Duration::from_secs(60),
            batch_limit: 1,
        };
        let p = PreAuthPurge {
            interval: Duration::from_secs(2),
            grace: Duration::ZERO,
            batch_limit: 1,
        };
        assert_eq!(
            (s.name(), s.interval()),
            ("session-purge", Duration::from_secs(1))
        );
        assert_eq!(
            (p.name(), p.interval()),
            ("pre-auth-purge", Duration::from_secs(2))
        );
    }
}
