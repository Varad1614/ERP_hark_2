//! Live Valkey tests. Run against a local valkey-server / redis-server:
//!
//! ```sh
//! SUTRA_TEST_VALKEY_URL=redis://127.0.0.1:6379 cargo test -p sutra-infra --features valkey-tests
//! ```
#![cfg(feature = "valkey-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;
use sutra_infra::valkey::{ValkeyCache, ValkeyReadiness, ValkeySettings};
use sutra_kernel::cache::{AdvisoryCache, Cache, CacheNamespace};
use sutra_kernel::health::ReadinessCheck;
use sutra_kernel::scope::{ExecutionScope, TenantId};

fn cache() -> Arc<ValkeyCache> {
    Arc::new(
        ValkeyCache::new(ValkeySettings {
            url: std::env::var("SUTRA_TEST_VALKEY_URL")
                .unwrap_or_else(|_| "redis://127.0.0.1:6379".into()),
            timeout: Duration::from_secs(1),
            cooldown: Duration::from_millis(100),
        })
        .unwrap(),
    )
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Item {
    name: String,
    version: i64,
}

#[tokio::test]
async fn roundtrip_ttl_delete_and_tenant_isolation() {
    let v = cache();
    assert!(ValkeyReadiness(v.clone()).check().await.is_ok());
    let c = AdvisoryCache::new(
        v.clone(),
        CacheNamespace::new("valkey-tests"),
        Duration::from_secs(30),
    );
    let a = ExecutionScope::tenant(TenantId::new());
    let b = ExecutionScope::tenant(TenantId::new());
    let ka = c.key("institution", &a, &["institution", "1"]);
    let kb = c.key("institution", &b, &["institution", "1"]);
    let item = Item {
        name: "Sutra".into(),
        version: 3,
    };
    c.put_json(&ka, &item).await;
    assert_eq!(c.get_json::<Item>(&ka).await, Some(item));
    assert_eq!(c.get_json::<Item>(&kb).await, None, "other tenant misses");
    c.invalidate(&ka).await;
    assert_eq!(c.get_json::<Item>(&ka).await, None);

    // TTL is enforced by the server.
    v.set(&ka, b"1", Duration::from_millis(50)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(v.get(&ka).await.unwrap(), None);

    // Garbage under a key decodes as a miss and is dropped.
    v.set(&ka, b"not json", Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(c.get_json::<Item>(&ka).await, None);
    assert_eq!(v.get(&ka).await.unwrap(), None);
}
