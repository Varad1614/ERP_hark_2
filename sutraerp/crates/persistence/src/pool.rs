use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgConnection, PgPool};
use std::str::FromStr;
use std::time::Duration;
use sutra_kernel::db_context::{self, DbContext};

#[derive(Clone, Debug)]
pub struct PoolSettings {
    pub url: String,
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout: Duration,
    pub application_name: String,
}

/// Statement copying a [`DbContext`] onto the database session (`$4` = transaction-local).
const SET_CONTEXT: &str = "SELECT set_config('sutra.tenant_id', $1, $4), \
     set_config('sutra.institution_id', $2, $4), set_config('sutra.rls_scope', $3, $4)";

/// Writes the row-level security settings of `ctx` on `conn` (Part 1 S3, DEF-X-003).
/// `local = true` limits them to the open transaction (`SET LOCAL` semantics).
pub async fn apply_context(
    conn: &mut PgConnection,
    ctx: Option<DbContext>,
    local: bool,
) -> Result<(), sqlx::Error> {
    let (tenant, institution, scope) = db_context::settings(ctx);
    sqlx::query(SET_CONTEXT)
        .bind(tenant)
        .bind(institution)
        .bind(scope)
        .bind(local)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Builds a bounded pool that connects lazily, so the API can start (and report not-ready)
/// while PostgreSQL is still coming up.
///
/// Part 1 S3 (DEF-X-003): every connection the pool hands out carries the row-level security
/// context of the acquiring task ([`db_context::current`]): it is written when a connection is
/// opened (`after_connect`) or taken from the idle queue (`before_acquire`, which also serves as
/// the liveness check `test_before_acquire` used to do), and cleared when it is returned
/// (`after_release`). A task without a context gets empty settings, which row-level security
/// treats as "no tenant" (fails closed).
pub fn connect_lazy(s: &PoolSettings) -> Result<PgPool, sqlx::Error> {
    let opts = PgConnectOptions::from_str(&s.url)?
        .application_name(&s.application_name)
        .options([("timezone", "UTC")]);
    Ok(PgPoolOptions::new()
        .max_connections(s.max_connections)
        .min_connections(s.min_connections)
        .acquire_timeout(s.acquire_timeout)
        .test_before_acquire(false)
        .after_connect(|conn, _meta| {
            let ctx = db_context::current();
            Box::pin(async move { apply_context(conn, ctx, false).await })
        })
        .before_acquire(|conn, _meta| {
            let ctx = db_context::current();
            Box::pin(async move {
                apply_context(conn, ctx, false).await?;
                Ok(true)
            })
        })
        .after_release(|conn, _meta| {
            Box::pin(async move {
                apply_context(conn, None, false).await?;
                Ok(true)
            })
        })
        .connect_lazy_with(opts))
}

/// Row-level-security relevant attributes of the role a pool connects as (Part 1 S3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleSecurity {
    pub role: String,
    pub superuser: bool,
    pub bypass_rls: bool,
}

impl RoleSecurity {
    /// `true` when PostgreSQL row-level security does not apply to this role.
    pub fn bypasses_rls(&self) -> bool {
        self.superuser || self.bypass_rls
    }
}

/// Reads [`RoleSecurity`] of the pool's login role. Neither attribute is inherited through role
/// membership, so the login role's own flags are the whole answer.
pub async fn role_security(pool: &PgPool) -> Result<RoleSecurity, sqlx::Error> {
    let (role, superuser, bypass_rls): (String, bool, bool) = sqlx::query_as(
        "SELECT rolname::text, rolsuper, rolbypassrls FROM pg_roles WHERE rolname = current_user",
    )
    .fetch_one(pool)
    .await?;
    Ok(RoleSecurity {
        role,
        superuser,
        bypass_rls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lazy_pool_does_not_need_a_server() {
        let pool = connect_lazy(&PoolSettings {
            url: "postgres://sutra:sutra@127.0.0.1:1/sutra".into(),
            max_connections: 2,
            min_connections: 0,
            acquire_timeout: Duration::from_millis(200),
            application_name: "test".into(),
        });
        assert!(pool.is_ok());
    }

    #[test]
    fn rejects_bad_url() {
        let r = connect_lazy(&PoolSettings {
            url: "not a url".into(),
            max_connections: 1,
            min_connections: 0,
            acquire_timeout: Duration::from_secs(1),
            application_name: "t".into(),
        });
        assert!(r.is_err());
    }
}
