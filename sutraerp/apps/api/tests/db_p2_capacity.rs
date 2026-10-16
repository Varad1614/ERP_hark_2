//! Part 2 · Stage 5 (S5-capacity) — regression tests for defects found while seeding and loading
//! the 2,000-student capacity institution (`docs/readiness/CAPACITY_TEST_REPORT.md`), on real
//! PostgreSQL (runtime role under RLS) with the real composition root:
//!
//! * `imported_students_are_provisioned_identities` — DEF-P2-CAP-001: students migrated through
//!   the data import (`POST /platform/imports`, entity `students`) are born `active`; the import
//!   now also publishes `student.status_changed`, so the identity jobs (S4-idp) create their
//!   Keycloak account (student role, institution group, `subject_ref` linked) exactly like a
//!   student enrolled through admissions. Before the fix no event reached `idp-events` and an
//!   imported student could never sign in.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use axum::Router;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use support::*;
use sutra_api::app::{router, AppContext};
use sutra_api::config::Config;
use sutra_contracts::platform::auth::{SessionPolicy, SessionStore};
use sutra_persistence::jobs::run_once;
use sutra_platform::auth::session_pg::PgSessionStore;
use sutra_platform::identity::memory::MemoryIdentityProvider;
use sutra_platform::identity::sync::{IdpEventConsumer, IdpSyncWorker};
use sutra_platform::identity::{IdentityRuntime, IdentitySettings};
use uuid::Uuid;

const T_IMPORT_IDP: &str = "0192f5a0-7c3e-7000-8000-0000000ca901";
const IMP: &str = "/api/v1/platform/imports";

/// Postgres-backed app (runtime role) in `tenant`, identity provisioning on (memory adapter).
async fn app(tenant: &'static str) -> (Router, sqlx::PgPool) {
    let owner = owner_pool().await;
    let url = database_url();
    let (scheme, rest) = url.split_once("://").unwrap();
    let host = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    let runtime: &'static str =
        Box::leak(format!("{scheme}://{RUNTIME_USER}:{RUNTIME_PASSWORD}@{host}").into_boxed_str());
    let src: HashMap<&'static str, &'static str> = HashMap::from([
        ("SUTRA_ENV", "development"),
        ("DATABASE_URL", runtime),
        ("SUTRA_SESSION_STORE", "postgres"),
        ("SUTRA_IDEMPOTENCY_STORE", "postgres"),
        ("SUTRA_AUDIT_SINK", "postgres"),
        ("SUTRA_DEV_TENANT_ID", tenant),
        ("SUTRA_SWEEPERS", "false"),
        ("SUTRA_INBOX_CONSUMER", "false"),
        ("SUTRA_APPROVAL_MODE", "auto"),
        ("SUTRA_IDP_PROVISIONING", "memory"),
    ]);
    let cfg = Config::load(&src).expect("config");
    let ctx = AppContext::from_config(&cfg).expect("context");
    (router(ctx), owner)
}

fn runtime(pool: &sqlx::PgPool, idp: Arc<MemoryIdentityProvider>, tenant: &str) -> IdentityRuntime {
    let sessions: Arc<dyn SessionStore> = Arc::new(PgSessionStore::new(
        pool.clone(),
        SessionPolicy::new(1800, 28800).unwrap(),
    ));
    IdentityRuntime {
        port: idp,
        sources: sutra_api::idp_wiring::identity_sources(pool),
        sessions,
        settings: IdentitySettings {
            auto_provision: true,
            retry_base: Duration::from_secs(30),
            retry_max: Duration::from_secs(60),
            tenants: Some(vec![tenant.parse().unwrap()]),
            ..IdentitySettings::default()
        },
    }
}

/// Standard base64 (with padding) — the import upload carries the file this way.
fn b64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            A[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

async fn call(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let (s, _, b) = send(app, write(who, method, path, body, None)).await;
    (s, b)
}

#[tokio::test]
async fn imported_students_are_provisioned_identities() {
    let (app, pool) = app(T_IMPORT_IDP).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup =
        support::b3::academic_setup(&app, &admin, T_IMPORT_IDP, "Capacity import identity").await;
    let inst = setup.institution_id;
    let tag = Uuid::now_v7().simple().to_string()[20..].to_uppercase();
    let numbers: Vec<String> = (1..=2).map(|i| format!("CAP{tag}/{i}")).collect();
    let emails: Vec<String> = (1..=2)
        .map(|i| format!("cap.{}.{i}@students.example.edu", tag.to_lowercase()))
        .collect();
    let mut file =
        String::from("student_number,given_name,family_name,date_of_birth,email,programme_code\n");
    for (i, (n, e)) in numbers.iter().zip(&emails).enumerate() {
        file.push_str(&format!(
            "{n},Migrated{i},Student,2005-04-1{i},{e},BTECH-CSE\n"
        ));
    }
    let (s, job) = call(
        &app,
        &admin,
        "POST",
        IMP,
        json!({"entity": "students", "institutionId": inst, "fileName": "students.csv",
               "contentBase64": b64(file.as_bytes())}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{job}");
    let jid = job["id"].as_str().unwrap().to_owned();
    let (s, j) = call(
        &app,
        &admin,
        "POST",
        &format!("{IMP}/{jid}/dry-run"),
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["status"], "validated", "{j}");
    let (s, j) = call(
        &app,
        &admin,
        "POST",
        &format!("{IMP}/{jid}/commit"),
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["status"], "committed", "{j}");

    let idp = Arc::new(MemoryIdentityProvider::new());
    let rt = runtime(&pool, idp.clone(), T_IMPORT_IDP);
    for (number, email) in numbers.iter().zip(&emails) {
        let sid: Uuid = sutra_kernel::db_context::system(async {
            sqlx::query_scalar(
                "SELECT id FROM student.student WHERE tenant_id = $1::uuid AND student_number = $2",
            )
            .bind(T_IMPORT_IDP)
            .bind(number)
            .fetch_one(&pool)
            .await
            .unwrap()
        })
        .await;
        // The migrated student's activation is announced like any other status change.
        let events: i64 = sutra_kernel::db_context::system(async {
            sqlx::query_scalar(
                "SELECT count(*) FROM platform.outbox WHERE subject = $1 AND event_type = $2 \
                 AND envelope -> 'data' ->> 'toStatus' = 'active'",
            )
            .bind(sid.to_string())
            .bind(sutra_contracts::student::events::STUDENT_STATUS_CHANGED)
            .fetch_one(&pool)
            .await
            .unwrap()
        })
        .await;
        assert_eq!(
            events, 1,
            "student.status_changed for imported student {number} ({email})"
        );
    }

    // idp-events → links queued; idp-sync → accounts created in the IdP.
    run_once(
        &pool,
        &IdpEventConsumer {
            runtime: rt.clone(),
        },
    )
    .await
    .expect("idp-events");
    run_once(
        &pool,
        &IdpSyncWorker {
            runtime: rt.clone(),
        },
    )
    .await
    .expect("idp-sync");
    assert_eq!(idp.user_count(), 2, "one identity per imported student");
    for (number, email) in numbers.iter().zip(&emails) {
        let l: Value = sutra_kernel::db_context::system(async {
            sqlx::query_scalar(
                "SELECT to_jsonb(l) FROM platform.identity_link l JOIN student.student s \
                 ON s.id = l.subject_id WHERE l.tenant_id = $1::uuid AND s.student_number = $2",
            )
            .bind(T_IMPORT_IDP)
            .bind(number)
            .fetch_one(&pool)
            .await
            .unwrap()
        })
        .await;
        assert_eq!(l["sync_status"], "synced", "{l}");
        assert_eq!(l["desired_state"], "active", "{l}");
        assert_eq!(l["username"], email.as_str(), "{l}");
        let uid = l["idp_user_id"].as_str().unwrap().to_owned();
        let u = idp.snapshot(&uid).unwrap();
        assert!(
            u.enabled && u.realm_roles.contains("student"),
            "account enabled, student role"
        );
        let paths: Vec<String> = u.groups.iter().filter_map(|g| idp.group_path(g)).collect();
        assert_eq!(
            paths,
            vec![format!("/sutra-tenant-{T_IMPORT_IDP}/institution-{inst}")]
        );
        let subject_ref: Option<String> = sutra_kernel::db_context::system(async {
            sqlx::query_scalar(
                "SELECT subject_ref FROM student.student \
                 WHERE tenant_id = $1::uuid AND student_number = $2",
            )
            .bind(T_IMPORT_IDP)
            .bind(number)
            .fetch_one(&pool)
            .await
            .unwrap()
        })
        .await;
        assert_eq!(
            subject_ref.as_deref(),
            Some(uid.as_str()),
            "self-service login linked"
        );
    }
    // Re-running the jobs (and re-importing the same file) creates nothing more.
    run_once(
        &pool,
        &IdpEventConsumer {
            runtime: rt.clone(),
        },
    )
    .await
    .expect("idp-events");
    run_once(&pool, &IdpSyncWorker { runtime: rt })
        .await
        .expect("idp-sync");
    assert_eq!(idp.user_count(), 2);
}
