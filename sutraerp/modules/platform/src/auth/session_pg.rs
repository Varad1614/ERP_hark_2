//! Durable PostgreSQL session store over `platform.session` (owned by Domain A).
//!
//! Expiry is evaluated with the database clock in the same statement that records activity, so
//! a lookup is a single atomic `UPDATE … RETURNING`: an idle-expired, absolutely-expired or
//! revoked session can never be revived by a concurrent request.

use super::session::{derive_csrf_token, new_session_token, token_hash};
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use sutra_contracts::platform::auth::{
    IssuedSession, Principal, Role, Session, SessionPolicy, SessionStore,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_kernel::secret;
use sutra_persistence::map_db_error;

const RETURNING: &str = "id, subject, login_id, display_name, roles, tenant_id, institution_id, \
                         csrf_secret, created_at, last_seen_at, expires_at";

#[derive(Clone)]
pub struct PgSessionStore {
    pool: PgPool,
    policy: SessionPolicy,
}

impl PgSessionStore {
    pub fn new(pool: PgPool, policy: SessionPolicy) -> Self {
        Self { pool, policy }
    }

    fn secs(d: chrono::Duration) -> f64 {
        // Whole seconds are exact in f64 for any realistic timeout.
        d.num_seconds() as f64
    }

    fn row_to_session(
        &self,
        row: &sqlx::postgres::PgRow,
        token: &str,
    ) -> Result<Session, KernelError> {
        let roles: Vec<String> = row.try_get("roles").map_err(map_db_error)?;
        // Deny by default: a role name this build does not know grants nothing.
        let roles = roles
            .iter()
            .filter_map(|r| {
                let parsed = Role::parse(r);
                if parsed.is_none() {
                    tracing::warn!(role = %r, "ignoring unknown role on stored session");
                }
                parsed
            })
            .collect();
        let tenant_id = TenantId(row.try_get("tenant_id").map_err(map_db_error)?);
        let institution_id: Option<uuid::Uuid> =
            row.try_get("institution_id").map_err(map_db_error)?;
        let csrf_secret: Vec<u8> = row.try_get("csrf_secret").map_err(map_db_error)?;
        let last_seen_at: DateTime<Utc> = row.try_get("last_seen_at").map_err(map_db_error)?;
        let expires_at: DateTime<Utc> = row.try_get("expires_at").map_err(map_db_error)?;
        Ok(Session {
            id: row.try_get("id").map_err(map_db_error)?,
            principal: Principal {
                subject: row.try_get("subject").map_err(map_db_error)?,
                display_name: row.try_get("display_name").map_err(map_db_error)?,
                login_id: row.try_get("login_id").map_err(map_db_error)?,
                roles,
                scope: ExecutionScope {
                    tenant_id,
                    institution_id: institution_id.map(InstitutionId),
                },
            },
            created_at: row.try_get("created_at").map_err(map_db_error)?,
            last_seen_at,
            expires_at,
            idle_expires_at: (last_seen_at + self.policy.idle_timeout).min(expires_at),
            csrf_token: derive_csrf_token(&csrf_secret, token)?,
        })
    }

    /// Deletes sessions that expired or were revoked more than `retention` ago. Returns the
    /// number of rows removed. (Scheduled by the sweeper in a later Batch 1 slice.)
    pub async fn purge_expired(&self, retention: chrono::Duration) -> Result<u64, KernelError> {
        let n = sqlx::query(
            "DELETE FROM platform.session \
             WHERE expires_at < now() - make_interval(secs => $1) \
                OR revoked_at < now() - make_interval(secs => $1) \
                OR last_seen_at < now() - make_interval(secs => $1 + $2)",
        )
        .bind(Self::secs(retention))
        .bind(Self::secs(self.policy.idle_timeout))
        .execute(&self.pool)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        Ok(n)
    }
}

#[async_trait::async_trait]
impl SessionStore for PgSessionStore {
    async fn create(&self, principal: Principal) -> Result<IssuedSession, KernelError> {
        let token = new_session_token()?;
        let csrf_secret = secret::random_bytes()?;
        let roles: Vec<&str> = principal.roles.iter().map(Role::as_str).collect();
        let row = sqlx::query(&format!(
            "INSERT INTO platform.session \
               (token_hash, csrf_secret, subject, login_id, display_name, roles, tenant_id, \
                institution_id, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now() + make_interval(secs => $9)) \
             RETURNING {RETURNING}"
        ))
        .bind(token_hash(&token).as_slice())
        .bind(csrf_secret.as_slice())
        .bind(&principal.subject)
        .bind(&principal.login_id)
        .bind(&principal.display_name)
        .bind(&roles)
        .bind(principal.scope.tenant_id.as_uuid())
        .bind(principal.scope.institution_id.map(|i| i.as_uuid()))
        .bind(Self::secs(self.policy.absolute_timeout))
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_error)?;
        let session = self.row_to_session(&row, &token)?;
        Ok(IssuedSession { token, session })
    }

    async fn get(&self, token: &str) -> Result<Option<Session>, KernelError> {
        let row = sqlx::query(&format!(
            "UPDATE platform.session SET last_seen_at = now() \
             WHERE token_hash = $1 \
               AND revoked_at IS NULL \
               AND expires_at > now() \
               AND last_seen_at > now() - make_interval(secs => $2) \
             RETURNING {RETURNING}"
        ))
        .bind(token_hash(token).as_slice())
        .bind(Self::secs(self.policy.idle_timeout))
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db_error)?;
        row.map(|r| self.row_to_session(&r, token)).transpose()
    }

    async fn revoke(&self, token: &str) -> Result<(), KernelError> {
        sqlx::query(
            "UPDATE platform.session SET revoked_at = now() \
             WHERE token_hash = $1 AND revoked_at IS NULL",
        )
        .bind(token_hash(token).as_slice())
        .execute(&self.pool)
        .await
        .map_err(map_db_error)?;
        Ok(())
    }
}
