//! Live OpenSearch 2.x + PostgreSQL 18 (Batch 9 · 9C, contract §8): rebuild → incremental
//! projection from outbox events → search through the API (tenant-filtered) → rebuild equals
//! incremental; a cross-tenant query returns nothing.
//!
//! ```sh
//! DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra SUTRA_TEST_OPENSEARCH_URL=http://localhost:9200 \
//!   cargo test -p sutra-api --features db-tests,opensearch-tests --test live_opensearch
//! ```
#![cfg(all(feature = "db-tests", feature = "opensearch-tests"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;
use support::{get, login, send};
use sutra_api::app::{router, AppContext, OpenSearchIndex};
use sutra_api::config::Config;
use sutra_contracts::platform::search::{SearchIndex, PROJECTIONS};
use sutra_kernel::events::{EventEnvelope, NewEvent};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::jobs::run_once;
use sutra_platform::search::projector::{SearchProjector, SearchRebuild};
use sutra_platform::search::SearchState;
use uuid::Uuid;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b9c00";
const TENANT_OTHER: &str = "0192f5a0-7c3e-7000-8000-0000000b9c01";

fn os_url() -> String {
    std::env::var("SUTRA_TEST_OPENSEARCH_URL").unwrap_or_else(|_| "http://localhost:9200".into())
}

fn tag() -> String {
    Uuid::now_v7().simple().to_string()[20..].to_lowercase()
}

fn digits() -> String {
    format!("{:09}", Uuid::now_v7().as_u128() % 1_000_000_000)
}

fn runtime_url() -> String {
    let url = support::database_url();
    let (scheme, rest) = url.split_once("://").unwrap();
    let host = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    format!(
        "{scheme}://{}:{}@{host}",
        support::RUNTIME_USER,
        support::RUNTIME_PASSWORD
    )
}

fn client(prefix: &str) -> sutra_infra::opensearch::OpenSearchClient {
    sutra_infra::opensearch::OpenSearchClient::new(sutra_infra::opensearch::OpenSearchSettings {
        url: os_url(),
        user: None,
        password: None,
        index_prefix: prefix.to_owned(),
        timeout: Duration::from_secs(30),
    })
    .unwrap()
}

/// API with `SUTRA_SEARCH=opensearch` (jobs not started: the test drives them with `run_once`).
fn search_app(tenant: &'static str, prefix: &'static str) -> axum::Router {
    let db: &'static str = Box::leak(runtime_url().into_boxed_str());
    let os: &'static str = Box::leak(os_url().into_boxed_str());
    let src: HashMap<&'static str, &'static str> = HashMap::from([
        ("SUTRA_ENV", "development"),
        ("DATABASE_URL", db),
        ("SUTRA_SESSION_STORE", "postgres"),
        ("SUTRA_IDEMPOTENCY_STORE", "postgres"),
        ("SUTRA_AUDIT_SINK", "postgres"),
        ("SUTRA_DEV_TENANT_ID", tenant),
        ("SUTRA_SWEEPERS", "false"),
        ("SUTRA_INBOX_CONSUMER", "false"),
        ("SUTRA_SEARCH", "opensearch"),
        ("SUTRA_OPENSEARCH_URL", os),
        ("SUTRA_OPENSEARCH_INDEX_PREFIX", prefix),
    ]);
    let cfg = Config::load(&src).expect("config");
    router(AppContext::from_config(&cfg).expect("context"))
}

async fn enqueue(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    institution: Uuid,
    event_type: &'static str,
    subject: Uuid,
    data: serde_json::Value,
) {
    let scope = ExecutionScope::institution(TenantId(tenant), InstitutionId(institution));
    let env = EventEnvelope::new(
        &scope,
        NewEvent {
            source: "/sutraerp/test-9c",
            event_type,
            dataschema: "urn:sutraerp:schema:test.9c:1",
            subject: Some(subject.to_string()),
            data,
        },
    );
    sutra_persistence::outbox::enqueue(pool, &env)
        .await
        .unwrap();
}

async fn seed_student(pool: &sqlx::PgPool, tenant: Uuid, inst: Uuid, given: &str) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO student.student (tenant_id, student_number, given_name, family_name, status) \
         VALUES ($1, $2, $3, 'Search', 'active') RETURNING id",
    )
    .bind(tenant)
    .bind(format!("S9C-{}", tag().to_uppercase()))
    .bind(given)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO student.student_context (tenant_id, student_id, institution_id, status) \
         VALUES ($1, $2, $3, 'active')",
    )
    .bind(tenant)
    .bind(id)
    .bind(inst)
    .execute(pool)
    .await
    .unwrap();
    enqueue(
        pool,
        tenant,
        inst,
        "sutraerp.student.student.created",
        id,
        serde_json::json!({ "studentId": id }),
    )
    .await;
    id
}

async fn seed_staff(pool: &sqlx::PgPool, tenant: Uuid, inst: Uuid, name: &str) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO people.staff_member (tenant_id, institution_id, staff_number, given_name, \
           display_name, staff_category, status) \
         VALUES ($1, $2, $3, $4, $4, 'teaching', 'active') RETURNING id",
    )
    .bind(tenant)
    .bind(inst)
    .bind(format!("STF-{}", digits()))
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap();
    enqueue(
        pool,
        tenant,
        inst,
        "sutraerp.people.staff_member.created",
        id,
        serde_json::json!({ "aggregateId": id }),
    )
    .await;
    id
}

async fn seed_record(pool: &sqlx::PgPool, tenant: Uuid, inst: Uuid, title: &str) -> (Uuid, Uuid) {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.catalogue_record (tenant_id, institution_id, title, authors, subjects, \
           status) VALUES ($1, $2, $3, ARRAY['Ada Lovelace'], ARRAY['computing'], 'published') \
         RETURNING id",
    )
    .bind(tenant)
    .bind(inst)
    .bind(title)
    .fetch_one(pool)
    .await
    .unwrap();
    let copy: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.library_copy (tenant_id, institution_id, record_id, accession_number) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(tenant)
    .bind(inst)
    .bind(id)
    .bind(format!("ACC-{}", digits()))
    .fetch_one(pool)
    .await
    .unwrap();
    enqueue(
        pool,
        tenant,
        inst,
        "sutraerp.campus.catalogue_record.published",
        id,
        serde_json::json!({ "aggregateId": id, "toStatus": "published" }),
    )
    .await;
    (id, copy)
}

async fn snapshot(index: &dyn SearchIndex, tenant: Uuid) -> Vec<BTreeMap<Uuid, serde_json::Value>> {
    let mut out = vec![];
    for p in PROJECTIONS {
        index.refresh(p).await.unwrap();
        let r = index.search(p, tenant, "", 100).await.unwrap();
        out.push(r.items.into_iter().map(|h| (h.id, h.document)).collect());
    }
    out
}

#[tokio::test]
async fn projections_search_rebuild_and_stay_inside_the_tenant() {
    let pool = support::owner_pool().await;
    let t = tag();
    let prefix: &'static str = Box::leak(format!("sutra-it-{t}").into_boxed_str());
    let tenant = Uuid::parse_str(TENANT).unwrap();
    let other = Uuid::parse_str(TENANT_OTHER).unwrap();
    let inst = Uuid::now_v7();

    let os = client(prefix);
    let mut ready = false;
    for _ in 0..60 {
        if os.health().await.is_ok() {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(ready, "OpenSearch did not become ready");
    let index: Arc<dyn SearchIndex> = Arc::new(OpenSearchIndex(os));
    let rt = SearchState::enabled(index.clone(), sutra_api::app::search_sources(pool.clone()))
        .runtime()
        .unwrap();
    let rebuild = SearchRebuild::new(rt.clone(), Duration::from_secs(1));
    let projector = SearchProjector {
        runtime: rt.clone(),
        interval: Duration::from_secs(1),
        batch_size: 10_000,
    };

    let ada = seed_student(&pool, tenant, inst, &format!("Ada{t}")).await;
    let grace = seed_staff(&pool, tenant, inst, &format!("Grace{t}")).await;
    let (record, copy) = seed_record(&pool, tenant, inst, &format!("Analytical Engines {t}")).await;
    let twin = seed_student(&pool, other, Uuid::now_v7(), &format!("Ada{t}")).await;

    // 1. Rebuild (first start: no checkpoints) creates <prefix>-<projection>-v1 + aliases.
    sqlx::query("DELETE FROM platform.search_checkpoint")
        .execute(&pool)
        .await
        .unwrap();
    run_once(&pool, &rebuild).await.unwrap();
    for p in PROJECTIONS {
        index.refresh(p).await.unwrap();
        let src = rt.source(p).unwrap();
        assert_eq!(
            index.count(p, None).await.unwrap(),
            src.count(None).await.unwrap(),
            "{p}: index == source after rebuild"
        );
    }

    // 2. Incremental: changes after the rebuild flow from the outbox.
    sqlx::query("UPDATE student.student SET given_name = $2, version = version + 1 WHERE id = $1")
        .bind(ada)
        .bind(format!("Augusta{t}"))
        .execute(&pool)
        .await
        .unwrap();
    enqueue(
        &pool,
        tenant,
        inst,
        "sutraerp.student.student.profile_updated",
        ada,
        serde_json::json!({ "aggregateId": ada, "version": 2, "changedFields": ["givenName"] }),
    )
    .await;
    sqlx::query(
        "UPDATE campus.library_copy SET status = 'issued', version = version + 1 WHERE id = $1",
    )
    .bind(copy)
    .execute(&pool)
    .await
    .unwrap();
    enqueue(
        &pool,
        tenant,
        inst,
        "sutraerp.campus.library_copy.status_changed",
        copy,
        serde_json::json!({ "aggregateId": copy, "toStatus": "issued" }),
    )
    .await;
    let mary = seed_student(&pool, tenant, inst, &format!("Mary{t}")).await;
    tokio::time::sleep(Duration::from_millis(2_300)).await; // projector SETTLE window
    run_once(&pool, &projector).await.unwrap();
    for p in PROJECTIONS {
        index.refresh(p).await.unwrap();
    }

    // 3. Search through the API (session tenant = TENANT).
    let app = search_app(TENANT, prefix);
    let admin = login(&app, "admin@sutra.dev").await;
    let q = |projection: &str, text: &str| {
        format!("/api/v1/platform/search?projection={projection}&q={text}")
    };
    let (s, _, body) = send(&app, get(&admin, &q("students", &format!("augusta{t}")))).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 1, "{body}");
    assert_eq!(body["items"][0]["id"], ada.to_string());
    assert_eq!(body["items"][0]["document"]["tenantId"], TENANT);
    let (s, _, body) = send(&app, get(&admin, &q("students", &format!("mary{t}")))).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["id"], mary.to_string(), "{body}");
    let (s, _, body) = send(&app, get(&admin, &q("staff", &format!("grace{t}")))).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["id"], grace.to_string(), "{body}");
    let (s, _, body) = send(
        &app,
        get(
            &admin,
            &q("library-catalogue", &format!("analytical%20engines%20{t}")),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["id"], record.to_string(), "{body}");
    assert_eq!(body["items"][0]["document"]["availableCopies"], 0, "{body}");

    // 4. Cross-tenant: tenant A never sees tenant B's twin, and vice versa.
    let (s, _, body) = send(&app, get(&admin, &q("students", &format!("ada{t}")))).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(
        body["total"], 0,
        "A's Ada was renamed; B's twin must not leak: {body}"
    );
    let b = index
        .search("students", other, &format!("augusta{t}"), 10)
        .await
        .unwrap();
    assert_eq!(b.total, 0, "B never sees A's documents");
    let b = index
        .search("students", other, &format!("ada{t}"), 10)
        .await
        .unwrap();
    assert_eq!(b.items.len(), 1);
    assert_eq!(b.items[0].id, twin);
    assert_eq!(b.items[0].document["tenantId"], TENANT_OTHER);

    // 5. Rebuild equals incremental.
    let incremental = snapshot(index.as_ref(), tenant).await;
    sqlx::query("UPDATE platform.search_checkpoint SET rebuild_requested_at = now()")
        .execute(&pool)
        .await
        .unwrap();
    run_once(&pool, &rebuild).await.unwrap();
    let rebuilt = snapshot(index.as_ref(), tenant).await;
    assert_eq!(incremental, rebuilt, "rebuild == incremental");
    assert!(incremental[0].contains_key(&ada) && incremental[0].contains_key(&mary));

    // 6. Replay of every event is idempotent (external_gte versions).
    sqlx::query("UPDATE platform.search_checkpoint SET last_outbox_id = NULL")
        .execute(&pool)
        .await
        .unwrap();
    run_once(&pool, &projector).await.unwrap();
    assert_eq!(
        snapshot(index.as_ref(), tenant).await,
        incremental,
        "replay"
    );

    // Clean up this run's indices.
    let cleanup = client(prefix);
    for p in PROJECTIONS {
        cleanup.delete_index(p).await.unwrap();
    }
}
