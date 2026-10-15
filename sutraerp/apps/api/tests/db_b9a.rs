//! Batch 9A — tamper-evident audit chain against a live PostgreSQL 18 (docs/batch9-contracts.md
//! §4): the sealer (batches, two tenants, concurrent sealers, late rows), `verify` (ok,
//! `hash_mismatch`, `link_broken`, `anchor_mismatch`), anchors, the runtime role's missing
//! UPDATE / DELETE privileges on the evidence tables, the canonical hash of a row read back from
//! PostgreSQL, and the three `platform-audit` routes with their permission.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use axum::Router;
use std::collections::HashMap;
use support::*;
use sutra_api::app::{router, AppContext};
use sutra_api::config::Config;
use sutra_kernel::audit::{ActorKind, AuditEntry, AuditOutcome};
use sutra_kernel::audit_chain::canonical_json;
use sutra_kernel::scope::ExecutionScope;
use sutra_persistence::audit_chain::{self, AnchorKey, BreakReason};
use uuid::Uuid;

const TENANT_A: &str = "0192f5a0-7c3e-7000-8000-0000000b9a00";
const TENANT_B: &str = "0192f5a0-7c3e-7000-8000-0000000b9a01";
/// Concurrency + canonical-hash tenant (kept apart so the other tests' counts stay exact).
const TENANT_C: &str = "0192f5a0-7c3e-7000-8000-0000000b9a02";
const ANCHOR_KEY_HEX: &str = "9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a";
const CHAIN: &str = "/api/v1/platform/audit/chain";

fn uuid(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap()
}

fn key(byte: u8) -> AnchorKey {
    AnchorKey {
        key: [byte; 32],
        key_id: "k1".into(),
    }
}

fn runtime_url() -> String {
    let url = database_url();
    let (scheme, rest) = url.split_once("://").unwrap();
    let host = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    format!("{scheme}://{RUNTIME_USER}:{RUNTIME_PASSWORD}@{host}")
}

/// Postgres-backed app (runtime role, dev auth in `tenant`) with the audit anchor key set.
async fn app_with_anchor_key(tenant: &'static str) -> (Router, sqlx::PgPool) {
    let owner = owner_pool().await;
    let url: &'static str = Box::leak(runtime_url().into_boxed_str());
    let src: HashMap<&'static str, &'static str> = HashMap::from([
        ("SUTRA_ENV", "development"),
        ("DATABASE_URL", url),
        ("SUTRA_SESSION_STORE", "postgres"),
        ("SUTRA_IDEMPOTENCY_STORE", "postgres"),
        ("SUTRA_AUDIT_SINK", "postgres"),
        ("SUTRA_DEV_TENANT_ID", tenant),
        ("SUTRA_SWEEPERS", "false"),
        ("SUTRA_INBOX_CONSUMER", "false"),
        ("SUTRA_AUDIT_ANCHOR_KEY", ANCHOR_KEY_HEX),
        ("SUTRA_AUDIT_ANCHOR_KEY_ID", "k1"),
    ]);
    let cfg = Config::load(&src).expect("config");
    let ctx = AppContext::from_config(&cfg).expect("context");
    (router(ctx), owner)
}

async fn runtime_pool() -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&runtime_url())
        .await
        .unwrap()
}

/// Inserts `n` audit rows for `tenant` through the shared insert path.
async fn insert_rows(pool: &sqlx::PgPool, tenant: &str, n: usize, tag: &str) -> Vec<Uuid> {
    let scope = ExecutionScope::parse(Some(tenant), None).unwrap();
    let mut ids = vec![];
    for i in 0..n {
        let e = AuditEntry::new(scope, format!("test.b9a.{tag}"), AuditOutcome::Succeeded)
            .actor(ActorKind::System, Some("db_b9a".into()))
            .target("b9a", format!("{tag}-{i}"))
            .details(serde_json::json!({
                "i": i,
                "tag": tag,
                "nested": {"z": [1, 2.5, -3], "a": "ünïcødé ✓"},
            }));
        ids.push(e.id);
        sutra_persistence::audit::insert(pool, &e).await.unwrap();
    }
    ids
}

async fn unsealed(pool: &sqlx::PgPool, tenant: &str) -> i64 {
    audit_chain::unsealed_count(pool, Some(uuid(tenant)))
        .await
        .unwrap()
}

/// Runs the sealer (batch `batch`) until `tenant` has nothing left to seal.
async fn seal_all(pool: &sqlx::PgPool, tenant: &str, batch: u32) -> u32 {
    let mut rounds = 0;
    while unsealed(pool, tenant).await > 0 {
        let r = audit_chain::seal_pending(pool, batch).await.unwrap();
        if r.skipped {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        rounds += 1;
        assert!(rounds < 10_000, "sealer does not converge");
    }
    rounds
}

/// `(count, min seq, max seq, distinct audit ids)` of a tenant's entries.
async fn chain_shape(pool: &sqlx::PgPool, tenant: &str) -> (i64, i64, i64, i64) {
    sqlx::query_as(
        "SELECT count(*), coalesce(min(seq), 0), coalesce(max(seq), 0), count(DISTINCT audit_id) \
         FROM platform.audit_chain_entry WHERE tenant_id = $1",
    )
    .bind(uuid(tenant))
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn audit_rows(pool: &sqlx::PgPool, tenant: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM platform.audit_log WHERE tenant_id = $1")
        .bind(uuid(tenant))
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn audit_id_at(pool: &sqlx::PgPool, tenant: &str, seq: i64) -> Uuid {
    sqlx::query_scalar(
        "SELECT audit_id FROM platform.audit_chain_entry WHERE tenant_id = $1 AND seq = $2",
    )
    .bind(uuid(tenant))
    .bind(seq)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn sealer_two_tenants_in_batches_api_and_anchors() {
    let (app, pool) = app_with_anchor_key(TENANT_A).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let student = login(&app, "aarav.desai@sutra.dev").await;
    insert_rows(&pool, TENANT_A, 11, "a").await;
    insert_rows(&pool, TENANT_B, 4, "b0").await;

    // ---- seal in batches of 3: every row of both tenants, gap-free, one entry per row.
    let mut ticks = 0;
    while unsealed(&pool, TENANT_A).await > 0 {
        let mut conn = pool.acquire().await.unwrap();
        let n = audit_chain::seal_tenant(&mut conn, uuid(TENANT_A), 3)
            .await
            .unwrap();
        assert!(n <= 3, "a tick seals at most one batch per tenant");
        ticks += 1;
        assert!(ticks < 1000);
    }
    seal_all(&pool, TENANT_B, 3).await;
    for t in [TENANT_A, TENANT_B] {
        let (count, min, max, distinct) = chain_shape(&pool, t).await;
        assert!(count > 0);
        assert_eq!(
            (min, max, distinct),
            (1, count, count),
            "{t} chain is gap-free"
        );
        let rows = audit_rows(&pool, t).await;
        assert!(count <= rows);
        let r = audit_chain::verify(&pool, uuid(t), None, None, None)
            .await
            .unwrap();
        assert!(r.ok, "{t}: {r:?}");
        assert_eq!(r.head_seq, max);
        assert_eq!(r.checked, u64::try_from(max).unwrap());
        // A sub-range verifies too.
        let part = audit_chain::verify(&pool, uuid(t), Some(2), Some(3), None)
            .await
            .unwrap();
        assert!(part.ok && part.checked == 2, "{part:?}");
    }

    // ---- API: status, permission, verify (audited), anchors.
    let (s, _, body) = send(&app, get(&admin, CHAIN)).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["tenantId"], TENANT_A);
    let head = body["headSeq"].as_i64().unwrap();
    assert!(head >= 11);
    assert_eq!(body["headHash"].as_str().unwrap().len(), 64);
    assert_eq!(body["sealed"].as_i64().unwrap(), head);
    assert!(body["lastSealedAt"].is_string());
    let (s, _, _) = send(&app, get(&student, CHAIN)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "students cannot read the chain");
    let (s, _, _) = send(
        &app,
        write(
            &student,
            "POST",
            &format!("{CHAIN}/verify"),
            serde_json::json!({}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let verifications_before: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.audit_log WHERE tenant_id = $1 \
         AND action = 'platform.audit.chain.verify'",
    )
    .bind(uuid(TENANT_A))
    .fetch_one(&pool)
    .await
    .unwrap();
    let (s, _, body) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{CHAIN}/verify"),
            serde_json::json!({}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true, "{body}");
    assert!(body["firstBrokenSeq"].is_null() && body["reason"].is_null());
    assert!(body["checked"].as_i64().unwrap() >= head);
    let verifications_after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.audit_log WHERE tenant_id = $1 \
         AND action = 'platform.audit.chain.verify'",
    )
    .bind(uuid(TENANT_A))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        verifications_after,
        verifications_before + 1,
        "verify is audited"
    );
    let (s, _, body) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{CHAIN}/verify"),
            serde_json::json!({"fromSeq": 5, "toSeq": 2}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // Anchors: sign every moved head with the configured key; the API lists them.
    seal_all(&pool, TENANT_A, 500).await;
    let configured = AnchorKey {
        key: [0x9a; 32],
        key_id: "k1".into(),
    };
    assert!(audit_chain::anchor(&pool, &configured).await.unwrap() >= 1);
    let anchors_of_a = |pool: sqlx::PgPool| async move {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM platform.audit_anchor WHERE tenant_id = $1 AND key_id = 'k1'",
        )
        .bind(uuid(TENANT_A))
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let before = anchors_of_a(pool.clone()).await;
    audit_chain::anchor(&pool, &configured).await.unwrap();
    assert_eq!(
        anchors_of_a(pool.clone()).await,
        before,
        "an unchanged head is not re-anchored"
    );
    let (s, _, body) = send(&app, get(&admin, "/api/v1/platform/audit/anchors?limit=5")).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let anchors = body.as_array().unwrap();
    assert!(!anchors.is_empty());
    assert_eq!(anchors[0]["keyId"], "k1");
    assert_eq!(anchors[0]["mac"].as_str().unwrap().len(), 64);
    let anchored_seq = anchors[0]["seq"].as_i64().unwrap();
    let (_, _, status) = send(&app, get(&admin, CHAIN)).await;
    assert_eq!(status["lastAnchor"]["seq"].as_i64().unwrap(), anchored_seq);

    // The API checks anchors with the configured key → still ok.
    let (s, _, body) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{CHAIN}/verify"),
            serde_json::json!({}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["ok"], true, "{body}");

    // ---- a wrong anchor key → anchor_mismatch at the anchored seq.
    let r = audit_chain::verify(&pool, uuid(TENANT_A), None, None, Some(&key(0x01)))
        .await
        .unwrap();
    assert!(!r.ok);
    assert_eq!(r.reason, Some(BreakReason::AnchorMismatch));
    assert_eq!(r.first_broken_seq, Some(anchored_seq));
    // Another key id is not checked.
    let other = AnchorKey {
        key: [0x01; 32],
        key_id: "k2".into(),
    };
    assert!(
        audit_chain::verify(&pool, uuid(TENANT_A), None, None, Some(&other))
            .await
            .unwrap()
            .ok
    );
}

#[tokio::test]
async fn tampering_is_detected_and_evidence_is_append_only() {
    let pool = owner_pool().await;
    let ids = insert_rows(&pool, TENANT_B, 8, "b1").await;
    assert_eq!(ids.len(), 8);
    seal_all(&pool, TENANT_B, 500).await;
    let ok = audit_chain::verify(&pool, uuid(TENANT_B), None, None, None)
        .await
        .unwrap();
    assert!(ok.ok, "{ok:?}");
    assert!(ok.head_seq >= 8);

    // ---- edit the details of the row sealed at seq 3 (owner pool, trigger disabled).
    let victim = audit_id_at(&pool, TENANT_B, 3).await;
    let original: serde_json::Value =
        sqlx::query_scalar("SELECT details FROM platform.audit_log WHERE id = $1")
            .bind(victim)
            .fetch_one(&pool)
            .await
            .unwrap();
    let set_details = |details: serde_json::Value| {
        let pool = pool.clone();
        async move {
            let mut tx = pool.begin().await.unwrap();
            sqlx::query(
                "ALTER TABLE platform.audit_log DISABLE TRIGGER audit_log_no_update_delete",
            )
            .execute(&mut *tx)
            .await
            .unwrap();
            sqlx::query("UPDATE platform.audit_log SET details = $2 WHERE id = $1")
                .bind(victim)
                .bind(details)
                .execute(&mut *tx)
                .await
                .unwrap();
            sqlx::query("ALTER TABLE platform.audit_log ENABLE TRIGGER audit_log_no_update_delete")
                .execute(&mut *tx)
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
    };
    let mut tampered = original.clone();
    tampered["i"] = serde_json::json!(999);
    set_details(tampered).await;
    let r = audit_chain::verify(&pool, uuid(TENANT_B), None, None, None)
        .await
        .unwrap();
    assert!(!r.ok);
    assert_eq!(r.first_broken_seq, Some(3), "{r:?}");
    assert_eq!(r.reason, Some(BreakReason::HashMismatch));
    assert_eq!(r.reason.unwrap().as_str(), "hash_mismatch");
    // A range before the tampered entry still verifies.
    assert!(
        audit_chain::verify(&pool, uuid(TENANT_B), Some(1), Some(2), None)
            .await
            .unwrap()
            .ok
    );
    // Restoring the exact value restores the chain.
    set_details(original).await;
    assert!(
        audit_chain::verify(&pool, uuid(TENANT_B), None, None, None)
            .await
            .unwrap()
            .ok
    );

    // ---- delete the chain entry at seq 5 (trigger disabled) → link_broken at 5.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(
        "ALTER TABLE platform.audit_chain_entry DISABLE TRIGGER audit_chain_entry_no_update_delete",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    let deleted =
        sqlx::query("DELETE FROM platform.audit_chain_entry WHERE tenant_id = $1 AND seq = 5")
            .bind(uuid(TENANT_B))
            .execute(&mut *tx)
            .await
            .unwrap()
            .rows_affected();
    sqlx::query(
        "ALTER TABLE platform.audit_chain_entry ENABLE TRIGGER audit_chain_entry_no_update_delete",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(deleted, 1);
    let r = audit_chain::verify(&pool, uuid(TENANT_B), None, None, None)
        .await
        .unwrap();
    assert!(!r.ok);
    assert_eq!(r.first_broken_seq, Some(5), "{r:?}");
    assert_eq!(r.reason, Some(BreakReason::LinkBroken));

    // ---- the runtime role cannot rewrite evidence (privileges, 42501), the owner hits the trigger.
    let runtime = runtime_pool().await;
    for sql in [
        "UPDATE platform.audit_chain_entry SET sealed_at = now() WHERE tenant_id = $1",
        "DELETE FROM platform.audit_chain_entry WHERE tenant_id = $1",
        "UPDATE platform.audit_anchor SET key_id = 'x' WHERE tenant_id = $1",
        "DELETE FROM platform.audit_anchor WHERE tenant_id = $1",
        "UPDATE platform.audit_log SET details = '{}' WHERE tenant_id = $1",
        "DELETE FROM platform.audit_chain_head WHERE tenant_id = $1",
    ] {
        let err = sqlx::query(sql)
            .bind(uuid(TENANT_B))
            .execute(&runtime)
            .await
            .expect_err(sql);
        let code = err
            .as_database_error()
            .and_then(|d| d.code())
            .map(|c| c.to_string());
        assert_eq!(code.as_deref(), Some("42501"), "{sql}: {err}");
    }
    let err =
        sqlx::query("UPDATE platform.audit_chain_entry SET sealed_at = now() WHERE tenant_id = $1")
            .bind(uuid(TENANT_B))
            .execute(&pool)
            .await
            .expect_err("append-only trigger");
    assert!(err.to_string().contains("append-only"), "{err}");
    // …but it can read and seal (SELECT / INSERT / head UPDATE).
    insert_rows(&runtime, TENANT_C, 1, "rt").await;
    let r = audit_chain::seal_pending(&runtime, 500).await;
    assert!(r.is_ok(), "{r:?}");
}

#[tokio::test]
async fn concurrent_sealers_produce_one_gap_free_chain() {
    let pool = owner_pool().await;
    insert_rows(&pool, TENANT_C, 30, "c0").await;
    // Two global sealers plus two tenant-level sealers (bypassing the global lock) race while
    // rows keep arriving.
    let mut tasks = vec![];
    for _ in 0..2 {
        let p = pool.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..20 {
                audit_chain::seal_pending(&p, 4).await.unwrap();
            }
        }));
    }
    for _ in 0..2 {
        let p = pool.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..20 {
                let mut conn = p.acquire().await.unwrap();
                audit_chain::seal_tenant(&mut conn, uuid(TENANT_C), 3)
                    .await
                    .unwrap();
            }
        }));
    }
    {
        let p = pool.clone();
        tasks.push(tokio::spawn(async move {
            insert_rows(&p, TENANT_C, 10, "c1").await;
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    seal_all(&pool, TENANT_C, 500).await;
    let (count, min, max, distinct) = chain_shape(&pool, TENANT_C).await;
    assert!(count >= 40);
    assert_eq!(
        (min, max, distinct),
        (1, count, count),
        "one gap-free chain"
    );
    let r = audit_chain::verify(&pool, uuid(TENANT_C), None, None, None)
        .await
        .unwrap();
    assert!(r.ok, "{r:?}");
    assert_eq!(r.head_seq, count);
}

#[tokio::test]
async fn canonical_hash_of_a_stored_row_is_stable() {
    let pool = owner_pool().await;
    let scope = ExecutionScope::parse(Some(TENANT_C), None).unwrap();
    let details = serde_json::json!({
        "zeta": {"b": [3, 2.5, -300, 12_345_678_901_i64, null, true], "a": "ü𝄞 \"q\" \\ /"},
        "émoji": "🙂",
        "alpha": {"nested": {"y": 1, "x": [{"k": "v", "a": 0}]}},
        "num": 1.0e-7,
    });
    let e = AuditEntry::new(scope, "test.b9a.canonical", AuditOutcome::Rejected)
        .actor(ActorKind::User, Some("admin".into()))
        .target("b9a", "canonical")
        .correlation(Some("corr-9a".into()))
        .details(details);
    sutra_persistence::audit::insert(&pool, &e).await.unwrap();
    let first = audit_chain::load_audit_row(&pool, e.id)
        .await
        .unwrap()
        .unwrap();
    let second = audit_chain::load_audit_row(&pool, e.id)
        .await
        .unwrap()
        .unwrap();
    let genesis = sutra_kernel::audit_chain::GENESIS_HASH;
    assert_eq!(first.hash(&genesis), second.hash(&genesis));
    let canonical = canonical_json(&first.as_chain_row());
    assert_eq!(canonical, canonical_json(&second.as_chain_row()));
    // Keys sorted at every level, timestamp with six fractional digits, UTC.
    let alpha = canonical.find("\"alpha\"").unwrap();
    let zeta = canonical.find("\"zeta\"").unwrap();
    assert!(alpha < zeta, "{canonical}");
    assert!(canonical.find("\"a\":\"ü𝄞").unwrap() < canonical.find("\"b\":[3").unwrap());
    let ts = canonical.split("\"occurredAt\":\"").nth(1).unwrap();
    assert_eq!(&ts[19..20], ".");
    assert_eq!(&ts[26..28], "Z\"");
    // The sealer stores exactly this hash chain for the row.
    seal_all(&pool, TENANT_C, 500).await;
    let (prev, stored): (Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT prev_hash, entry_hash FROM platform.audit_chain_entry WHERE audit_id = $1",
    )
    .bind(e.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let prev: [u8; 32] = prev.try_into().unwrap();
    assert_eq!(first.hash(&prev).to_vec(), stored);
}
