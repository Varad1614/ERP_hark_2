//! Part 2 · Stage 5 (S5-monitor) DB test: the retry-backlog gauges exported on `/metrics`
//! (`sutra_idp_sync_links{status}`, `sutra_gateway_payments_open_stale`) are computed on real
//! PostgreSQL **as the runtime role**, across tenants (system context), and match what the
//! superuser sees. Without the system context row-level security hides every tenant row, which is
//! why the collector must run as server-internal work.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use support::*;
use sutra_api::metrics::{global, refresh_backlog_gauges, StatusLabels, IDP_SYNC_STATES};
use uuid::Uuid;

const IN_TENANTS: &str = "SELECT count(*) FROM platform.identity_link WHERE tenant_id = ANY($1)";
const BY_STATUS: &str = "SELECT count(*) FROM platform.identity_link WHERE sync_status = $1";
const STALE: &str = "SELECT count(*) FROM finance.payment WHERE channel = 'gateway' \
    AND status IN ('initiated', 'pending') AND created_at < now() - interval '1 hour'";
const RESOLVE: &str =
    "UPDATE platform.identity_link SET sync_status = 'synced' WHERE tenant_id = ANY($1)";

fn runtime_pool() -> sqlx::PgPool {
    let url = database_url();
    let (scheme, rest) = url.split_once("://").unwrap();
    let host = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    sutra_persistence::pool::connect_lazy(&sutra_persistence::pool::PoolSettings {
        url: format!("{scheme}://{RUNTIME_USER}:{RUNTIME_PASSWORD}@{host}"),
        max_connections: 2,
        min_connections: 0,
        acquire_timeout: std::time::Duration::from_secs(10),
        application_name: "sutra-api-db-tests-monitor".into(),
    })
    .unwrap()
}

async fn insert_link(owner: &sqlx::PgPool, tenant: Uuid, status: &str) {
    let tag = Uuid::now_v7().simple().to_string();
    sqlx::query(
        "INSERT INTO platform.identity_link (tenant_id, subject_type, subject_id, idp, username, \
           given_name, erp_state, desired_state, sync_status, attempts) \
         VALUES ($1, 'student', $2, 'memory', $3, 'Monitor', 'active', 'active', $4, 3)",
    )
    .bind(tenant)
    .bind(Uuid::now_v7())
    .bind(format!("mon-{tag}"))
    .bind(status)
    .execute(owner)
    .await
    .unwrap();
}

fn gauge(status: &str) -> i64 {
    global()
        .idp_sync_links
        .get_or_create(&StatusLabels {
            status: status.to_owned(),
        })
        .get()
}

#[tokio::test]
async fn backlog_gauges_count_every_tenant_as_the_runtime_role() {
    let owner = owner_pool().await;
    // Two tenants, so the count is provably cross-tenant.
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    insert_link(&owner, a, "failed").await;
    insert_link(&owner, b, "failed").await;
    insert_link(&owner, b, "conflict").await;
    insert_link(&owner, a, "synced").await;

    let runtime = runtime_pool();
    // Fail-closed baseline: without a database context the runtime role sees no tenant rows.
    let hidden: i64 = sqlx::query_scalar(IN_TENANTS)
        .bind(vec![a, b])
        .fetch_one(&runtime)
        .await
        .unwrap();
    assert_eq!(hidden, 0, "RLS must hide tenant rows outside a context");

    refresh_backlog_gauges(&runtime).await.unwrap();

    for status in IDP_SYNC_STATES {
        let truth: i64 = sqlx::query_scalar(BY_STATUS)
            .bind(status)
            .fetch_one(&owner)
            .await
            .unwrap();
        assert_eq!(
            gauge(status),
            truth,
            "sutra_idp_sync_links{{status={status}}}"
        );
    }
    assert!(gauge("failed") >= 2);
    assert!(gauge("conflict") >= 1);

    let stale: i64 = sqlx::query_scalar(STALE).fetch_one(&owner).await.unwrap();
    assert_eq!(global().gateway_open_stale.get(), stale);

    // The exposition carries the families with bounded labels only.
    let text = global().render();
    assert!(
        text.contains("sutra_idp_sync_links{status=\"failed\"}"),
        "{text}"
    );
    assert!(text.contains("# TYPE sutra_gateway_payments_open_stale gauge"));
    assert!(!text.contains(&a.to_string()));

    // Fixing the backlog brings the gauge down on the next refresh.
    sqlx::query(RESOLVE)
        .bind(vec![a, b])
        .execute(&owner)
        .await
        .unwrap();
    let before = gauge("failed");
    refresh_backlog_gauges(&runtime).await.unwrap();
    assert_eq!(gauge("failed"), before - 2);
}
