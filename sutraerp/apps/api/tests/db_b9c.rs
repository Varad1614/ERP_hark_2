//! Batch 9 · slice 9C DB tests (PostgreSQL 18, no Flowable / OpenSearch needed):
//!
//! * the search API answers `503 search_disabled` when `SUTRA_SEARCH=off`, validates the
//!   projection and checks the projection's read permission;
//! * the committed BPMN files equal the adapter's rendering and cover every approval key;
//! * the projector + rebuild jobs against real Domain C / E / G tables with an in-memory
//!   `SearchIndex` (external-version semantics): rebuild, incremental projection from outbox
//!   events, tenant separation, replay idempotence, rebuild == incremental;
//! * `SUTRA_WORKFLOW=flowable` / `SUTRA_SEARCH=opensearch` wire their background jobs.
//!
//! ```sh
//! DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra \
//!   cargo test -p sutra-api --features db-tests --test db_b9c
//! ```
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use support::{get, login, send};
use sutra_contracts::platform::search::{
    SearchDocument, SearchHit, SearchIndex, SearchResults, PROJECTIONS,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::events::{EventEnvelope, NewEvent};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::jobs::{run_once, JobOutcome};
use sutra_platform::search::projector::{SearchProjector, SearchRebuild};
use sutra_platform::search::SearchState;
use uuid::Uuid;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b9c00";
const TENANT_OTHER: &str = "0192f5a0-7c3e-7000-8000-0000000b9c01";

// ------------------------------------------------------------------ in-memory SearchIndex

type Docs = HashMap<(String, Uuid), (i64, Uuid, serde_json::Value)>;

/// `SearchIndex` with OpenSearch's `external_gte` semantics (a lower version is ignored).
#[derive(Default)]
struct MemIndex {
    docs: Mutex<Docs>,
}

impl MemIndex {
    fn tenant_docs(&self, projection: &str, tenant: Uuid) -> BTreeMap<Uuid, serde_json::Value> {
        self.docs
            .lock()
            .unwrap()
            .iter()
            .filter(|((p, _), (_, t, _))| p == projection && *t == tenant)
            .map(|((_, id), (_, _, body))| (*id, body.clone()))
            .collect()
    }

    fn version(&self, projection: &str, id: Uuid) -> Option<i64> {
        self.docs
            .lock()
            .unwrap()
            .get(&(projection.to_owned(), id))
            .map(|(v, _, _)| *v)
    }
}

#[async_trait::async_trait]
impl SearchIndex for MemIndex {
    async fn ready(&self) -> Result<(), KernelError> {
        Ok(())
    }
    async fn ensure(&self, _projection: &str) -> Result<(), KernelError> {
        Ok(())
    }
    async fn recreate(&self, projection: &str) -> Result<(), KernelError> {
        self.docs
            .lock()
            .unwrap()
            .retain(|(p, _), _| p != projection);
        Ok(())
    }
    async fn upsert(&self, projection: &str, docs: &[SearchDocument]) -> Result<u64, KernelError> {
        let mut m = self.docs.lock().unwrap();
        let mut n = 0;
        for d in docs {
            let key = (projection.to_owned(), d.id);
            if m.get(&key).is_some_and(|(v, _, _)| *v > d.version) {
                continue;
            }
            m.insert(key, (d.version, d.tenant_id, d.body.clone()));
            n += 1;
        }
        Ok(n)
    }
    async fn delete(&self, projection: &str, ids: &[Uuid]) -> Result<u64, KernelError> {
        let mut m = self.docs.lock().unwrap();
        Ok(ids
            .iter()
            .filter(|id| m.remove(&(projection.to_owned(), **id)).is_some())
            .count() as u64)
    }
    async fn refresh(&self, _projection: &str) -> Result<(), KernelError> {
        Ok(())
    }
    async fn search(
        &self,
        projection: &str,
        tenant: Uuid,
        q: &str,
        limit: u32,
    ) -> Result<SearchResults, KernelError> {
        let q = q.to_lowercase();
        let items: Vec<SearchHit> = self
            .tenant_docs(projection, tenant)
            .into_iter()
            .filter(|(_, b)| q.is_empty() || b.to_string().to_lowercase().contains(&q))
            .take(limit as usize)
            .map(|(id, document)| SearchHit {
                id,
                projection: projection.to_owned(),
                score: 1.0,
                document,
            })
            .collect();
        Ok(SearchResults {
            projection: projection.to_owned(),
            total: items.len() as u64,
            items,
        })
    }
    async fn count(&self, projection: &str, tenant: Option<Uuid>) -> Result<u64, KernelError> {
        Ok(self
            .docs
            .lock()
            .unwrap()
            .iter()
            .filter(|((p, _), (_, t, _))| p == projection && tenant.is_none_or(|x| x == *t))
            .count() as u64)
    }
}

// ------------------------------------------------------------------ seeding helpers

fn tag() -> String {
    Uuid::now_v7().simple().to_string()[20..].to_uppercase()
}

fn digits() -> String {
    let n = Uuid::now_v7().as_u128() % 1_000_000_000;
    format!("{n:09}")
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

/// Student persona + one StudentContext; returns the student id.
async fn seed_student(pool: &sqlx::PgPool, tenant: Uuid, institution: Uuid, given: &str) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO student.student (tenant_id, student_number, given_name, family_name, status) \
         VALUES ($1, $2, $3, 'Search', 'active') RETURNING id",
    )
    .bind(tenant)
    .bind(format!("S9C-{}", tag()))
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
    .bind(institution)
    .execute(pool)
    .await
    .unwrap();
    enqueue(
        pool,
        tenant,
        institution,
        "sutraerp.student.student.created",
        id,
        serde_json::json!({ "studentId": id }),
    )
    .await;
    id
}

async fn seed_staff(pool: &sqlx::PgPool, tenant: Uuid, institution: Uuid, name: &str) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO people.staff_member (tenant_id, institution_id, staff_number, given_name, \
           display_name, staff_category, status) \
         VALUES ($1, $2, $3, $4, $4, 'teaching', 'active') RETURNING id",
    )
    .bind(tenant)
    .bind(institution)
    .bind(format!("STF-{}", digits()))
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap();
    enqueue(
        pool,
        tenant,
        institution,
        "sutraerp.people.staff_member.created",
        id,
        serde_json::json!({ "aggregateId": id }),
    )
    .await;
    id
}

/// Catalogue record with `copies` available copies; returns (record id, copy ids).
async fn seed_record(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    institution: Uuid,
    title: &str,
    copies: usize,
) -> (Uuid, Vec<Uuid>) {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.catalogue_record (tenant_id, institution_id, title, authors, subjects, \
           status) VALUES ($1, $2, $3, ARRAY['Ada Lovelace'], ARRAY['computing'], 'published') \
         RETURNING id",
    )
    .bind(tenant)
    .bind(institution)
    .bind(title)
    .fetch_one(pool)
    .await
    .unwrap();
    let mut ids = vec![];
    for _ in 0..copies {
        let c: Uuid = sqlx::query_scalar(
            "INSERT INTO campus.library_copy (tenant_id, institution_id, record_id, \
               accession_number) VALUES ($1, $2, $3, $4) RETURNING id",
        )
        .bind(tenant)
        .bind(institution)
        .bind(id)
        .bind(format!("ACC-{}", digits()))
        .fetch_one(pool)
        .await
        .unwrap();
        ids.push(c);
    }
    enqueue(
        pool,
        tenant,
        institution,
        "sutraerp.campus.catalogue_record.published",
        id,
        serde_json::json!({ "aggregateId": id, "toStatus": "published" }),
    )
    .await;
    (id, ids)
}

fn runtime(index: Arc<MemIndex>, pool: sqlx::PgPool) -> Arc<sutra_platform::search::SearchRuntime> {
    SearchState::enabled(index, sutra_api::app::search_sources(pool))
        .runtime()
        .unwrap()
}

// ------------------------------------------------------------------ tests

#[tokio::test]
async fn search_api_is_disabled_by_default_and_checks_permissions() {
    let (app, _pool) = support::app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let (s, _, body) = send(
        &app,
        get(&admin, "/api/v1/platform/search?projection=students&q=aar"),
    )
    .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["errorCode"], "search_disabled");
    let (s, _, body) = send(
        &app,
        get(&admin, "/api/v1/platform/search?projection=grades"),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let (s, _, body) = send(
        &app,
        get(&student, "/api/v1/platform/search?projection=staff"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
    let (s, _, _) = send(
        &app,
        axum::http::Request::get("/api/v1/platform/search?projection=students")
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[test]
fn bpmn_files_match_the_adapter_and_cover_every_approval_key() {
    use sutra_platform::workflow_flowable::{bpmn_xml, PROCESS_KEYS};
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/flowable/processes");
    let files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".bpmn20.xml"))
        .collect();
    assert_eq!(files.len(), PROCESS_KEYS.len(), "one BPMN per approval key");
    for k in PROCESS_KEYS {
        let committed = std::fs::read_to_string(dir.join(format!("{k}.bpmn20.xml")))
            .unwrap_or_else(|_| panic!("missing deploy/flowable/processes/{k}.bpmn20.xml"));
        assert_eq!(
            committed,
            bpmn_xml(k),
            "{k}: run deploy/flowable/gen-bpmn.py"
        );
    }
    let mut module_keys: Vec<&str> = vec![];
    module_keys.extend_from_slice(sutra_student::shared::approval::process::ALL);
    module_keys.extend_from_slice(sutra_academic::shared::approval::process::ALL);
    module_keys.extend_from_slice(sutra_people::shared::approval::process::ALL);
    module_keys.extend_from_slice(sutra_finance::shared::approval::process::ALL);
    module_keys.extend_from_slice(sutra_campus::shared::approval::process::ALL);
    module_keys.extend_from_slice(sutra_services::shared::approval::process::ALL);
    module_keys.extend_from_slice(sutra_exams::shared::approval::process::ALL);
    module_keys.extend_from_slice(sutra_resources::shared::approval::process::ALL);
    {
        use sutra_institution::shared::approval::process as i;
        module_keys.extend_from_slice(&[
            i::INSTITUTION_ACTIVATION,
            i::ORG_RESTRUCTURE,
            i::CAMPUS_ACTIVATION,
            i::CURRICULUM_PUBLICATION,
            i::CALENDAR_PUBLICATION,
            i::ACADEMIC_YEAR_CLOSURE,
        ]);
    }
    // Part 2 · S5-capacity, DEF-P2-CAP-004: the data import's commit approval (opening fee
    // balances) is deployed to Flowable too — it was missing, so every governed commit failed
    // with 500 in production mode.
    module_keys.push(sutra_platform::dataimport::process::COMMIT_APPROVAL);
    for k in module_keys {
        assert!(
            PROCESS_KEYS.contains(&k),
            "approval key {k} has no BPMN: run deploy/flowable/gen-bpmn.py"
        );
    }
}

#[tokio::test]
async fn hardening_modes_wire_their_background_jobs() {
    let base = [
        ("SUTRA_ENV", "test"),
        ("DATABASE_URL", "postgres://sutra:sutra@127.0.0.1:1/sutra"),
        ("SUTRA_DB_ACQUIRE_TIMEOUT_SECS", "1"),
    ];
    let mut src: HashMap<&'static str, &'static str> = HashMap::from(base);
    let mut ctx =
        sutra_api::app::AppContext::from_config(&sutra_api::config::Config::load(&src).unwrap())
            .unwrap();
    let jobs = ctx.take_background().describe();
    assert!(!jobs
        .iter()
        .any(|j| j.starts_with("search_") || j.starts_with("workflow_")));
    assert!(!ctx.search.is_enabled());

    src.insert("SUTRA_WORKFLOW", "flowable");
    src.insert("SUTRA_FLOWABLE_URL", "http://127.0.0.1:1/flowable-rest");
    src.insert("SUTRA_SEARCH", "opensearch");
    src.insert("SUTRA_OPENSEARCH_URL", "http://127.0.0.1:1");
    let mut ctx =
        sutra_api::app::AppContext::from_config(&sutra_api::config::Config::load(&src).unwrap())
            .unwrap();
    let jobs = ctx.take_background().describe();
    for j in ["workflow_decision_sync", "search_project", "search_rebuild"] {
        assert!(jobs.iter().any(|x| x == j), "{j} missing from {jobs:?}");
    }
    assert!(ctx.search.is_enabled());
}

#[tokio::test]
async fn projector_and_rebuild_keep_documents_tenant_scoped_and_idempotent() {
    let pool = support::owner_pool().await;
    let tenant = Uuid::parse_str(TENANT).unwrap();
    let other = Uuid::parse_str(TENANT_OTHER).unwrap();
    let inst = Uuid::now_v7();
    let inst_other = Uuid::now_v7();
    let t = tag();
    let ada = seed_student(&pool, tenant, inst, &format!("Ada{t}")).await;
    let staff = seed_staff(&pool, tenant, inst, &format!("Grace{t}")).await;
    let (record, copies) = seed_record(&pool, tenant, inst, &format!("Engines {t}"), 2).await;
    let twin = seed_student(&pool, other, inst_other, &format!("Ada{t}")).await;

    let index = Arc::new(MemIndex::default());
    let rt = runtime(index.clone(), pool.clone());
    let rebuild = SearchRebuild::new(rt.clone(), Duration::from_secs(1));
    let projector = SearchProjector {
        runtime: rt.clone(),
        interval: Duration::from_secs(1),
        batch_size: 10_000,
    };

    // 1. First start: no checkpoint → the rebuild job re-reads everything.
    sqlx::query("DELETE FROM platform.search_checkpoint")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        run_once(&pool, &rebuild).await.unwrap(),
        JobOutcome::Ran { .. }
    ));
    let rows: Vec<(String, Option<i64>)> = sqlx::query_as(
        "SELECT projection, last_rebuild_count FROM platform.search_checkpoint ORDER BY projection",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), PROJECTIONS.len());
    for p in PROJECTIONS {
        let src = rt.source(p).unwrap();
        assert_eq!(
            index.count(p, Some(tenant)).await.unwrap(),
            src.count(Some(tenant)).await.unwrap(),
            "{p}: index == source after rebuild"
        );
    }
    let students = index.tenant_docs("students", tenant);
    assert_eq!(students[&ada]["displayName"], format!("Ada{t} Search"));
    assert_eq!(students[&ada]["tenantId"], TENANT);
    assert_eq!(students[&ada]["institutionId"], inst.to_string());
    assert!(
        !students.contains_key(&twin),
        "tenant B's twin never in tenant A"
    );
    assert_eq!(
        index.tenant_docs("library-catalogue", tenant)[&record]["availableCopies"],
        2
    );
    assert_eq!(
        index.tenant_docs("staff", tenant)[&staff]["number"]
            .as_str()
            .unwrap()
            .len(),
        13
    );

    // 2. Changes after the rebuild arrive through the outbox.
    let v_before = index.version("students", ada).unwrap();
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
    .bind(copies[0])
    .execute(&pool)
    .await
    .unwrap();
    enqueue(
        &pool,
        tenant,
        inst,
        "sutraerp.campus.library_copy.status_changed",
        copies[0],
        serde_json::json!({ "aggregateId": copies[0], "toStatus": "issued" }),
    )
    .await;
    let late = seed_student(&pool, tenant, inst, &format!("Mary{t}")).await;
    // Unrelated event types are skipped by every projection.
    enqueue(
        &pool,
        tenant,
        inst,
        "sutraerp.campus.library_fine.raised",
        record,
        serde_json::json!({ "aggregateId": record }),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(2_300)).await; // projector SETTLE window
    run_once(&pool, &projector).await.unwrap();
    let students = index.tenant_docs("students", tenant);
    assert_eq!(students[&ada]["displayName"], format!("Augusta{t} Search"));
    assert!(index.version("students", ada).unwrap() > v_before);
    assert!(students.contains_key(&late), "new student projected");
    assert_eq!(
        index.tenant_docs("library-catalogue", tenant)[&record]["availableCopies"],
        1
    );
    let moved: Option<Uuid> = sqlx::query_scalar(
        "SELECT last_outbox_id FROM platform.search_checkpoint WHERE projection = 'students'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(moved.is_some(), "checkpoint moved");

    // 3. Replay from scratch is idempotent (external versions).
    let incremental: Vec<_> = PROJECTIONS
        .iter()
        .map(|p| index.tenant_docs(p, tenant))
        .collect();
    sqlx::query("UPDATE platform.search_checkpoint SET last_outbox_id = NULL")
        .execute(&pool)
        .await
        .unwrap();
    run_once(&pool, &projector).await.unwrap();
    let replayed: Vec<_> = PROJECTIONS
        .iter()
        .map(|p| index.tenant_docs(p, tenant))
        .collect();
    assert_eq!(incremental, replayed, "replay changes nothing");

    // 4. Rebuild on request equals the incremental state.
    sqlx::query("UPDATE platform.search_checkpoint SET rebuild_requested_at = now()")
        .execute(&pool)
        .await
        .unwrap();
    run_once(&pool, &rebuild).await.unwrap();
    let rebuilt: Vec<_> = PROJECTIONS
        .iter()
        .map(|p| index.tenant_docs(p, tenant))
        .collect();
    assert_eq!(incremental, rebuilt, "rebuild == incremental");
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.search_checkpoint WHERE rebuild_requested_at IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pending, 0);

    // 5. Search stays inside the tenant.
    let hits = rt
        .index
        .search("students", other, &format!("Ada{t}"), 10)
        .await
        .unwrap();
    assert_eq!(hits.items.len(), 1);
    assert_eq!(hits.items[0].id, twin);
}
