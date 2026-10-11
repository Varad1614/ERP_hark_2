//! Embedded migrations (`/migrations`). Executed by `sutra-api migrate` — a separate step from
//! normal serving, per ADR-L5-0008 §3.4. Applied history is immutable; fixes move forward.

use sqlx::migrate::Migrator;
use sqlx::PgPool;

pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

pub async fn run(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    MIGRATOR.run(pool).await
}

/// Group role every runtime login is granted (created by the runtime-roles migration).
pub const RUNTIME_GROUP_ROLE: &str = "sutra_app";

/// Creates (or updates the password of) the API's runtime LOGIN role and grants it
/// [`RUNTIME_GROUP_ROLE`]. The role owns nothing and inherits only the module grants
/// (ADR-L5-0005/0036). Needs CREATEROLE on the migrating connection; meant for development and
/// compose — production provisions the login outside the application. Idempotent and safe to
/// run concurrently (role names are cluster-wide).
pub async fn provision_runtime_login(
    pool: &PgPool,
    user: &str,
    password: &str,
) -> Result<(), sqlx::Error> {
    if user.is_empty() || user == RUNTIME_GROUP_ROLE || user.starts_with("pg_") {
        return Err(sqlx::Error::Protocol(format!(
            "refusing to provision runtime role `{user}`"
        )));
    }
    let mut conn = pool.acquire().await?;
    // Serialise concurrent provisioning (cluster-wide catalog) with a session advisory lock.
    sqlx::query("SELECT pg_advisory_lock(hashtext('sutra.provision_runtime_login'))")
        .execute(&mut *conn)
        .await?;
    let result = async {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1)")
                .bind(user)
                .fetch_one(&mut *conn)
                .await?;
        // Identifiers / literals are quoted server-side with format(%I / %L).
        let ddl: String = sqlx::query_scalar(if exists {
            "SELECT format('ALTER ROLE %I WITH LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD %L', $1::text, $2::text)"
        } else {
            "SELECT format('CREATE ROLE %I WITH LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD %L', $1::text, $2::text)"
        })
        .bind(user)
        .bind(password)
        .fetch_one(&mut *conn)
        .await?;
        sqlx::query(&ddl).execute(&mut *conn).await?;
        let grant: String = sqlx::query_scalar("SELECT format('GRANT %I TO %I', $1::text, $2::text)")
            .bind(RUNTIME_GROUP_ROLE)
            .bind(user)
            .fetch_one(&mut *conn)
            .await?;
        sqlx::query(&grant).execute(&mut *conn).await?;
        Ok::<(), sqlx::Error>(())
    }
    .await;
    let _ = sqlx::query("SELECT pg_advisory_unlock(hashtext('sutra.provision_runtime_login'))")
        .execute(&mut *conn)
        .await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_embedded_and_ordered() {
        let versions: Vec<i64> = MIGRATOR.iter().map(|m| m.version).collect();
        assert!(versions.len() >= 5);
        let mut sorted = versions.clone();
        sorted.sort_unstable();
        assert_eq!(versions, sorted);
    }

    #[test]
    fn migrations_never_enable_extensions() {
        // ADR-L5-0036 §3.8: extensions are deny-by-default.
        for m in MIGRATOR.iter() {
            assert!(
                !m.sql.to_lowercase().contains("create extension"),
                "{}",
                m.description
            );
        }
    }
}
