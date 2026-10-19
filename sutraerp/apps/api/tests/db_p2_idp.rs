//! Part 2 · Stage 4.3 (S4-idp) — identity provisioning on real PostgreSQL with the in-memory IdP
//! adapter (the live Keycloak run is `live_keycloak_idp.rs`):
//!
//! * `student_lifecycle_drives_the_identity` — a `student.status_changed` event auto-provisions
//!   the identity (VERIFY_EMAIL + UPDATE_PASSWORD, `student` realm role, tenant/institution group,
//!   `subject_ref` linked); suspension through the ERP API disables the account, ends IdP and
//!   SutraERP sessions; restoration re-enables it; every step is audited.
//! * `duplicate_identities_are_prevented` — provisioning twice is idempotent; a second student
//!   with the same e-mail is refused (`409`); an IdP account that pre-exists is adopted instead of
//!   duplicated; an account of another tenant is never adopted (`conflict`).
//! * `retry_after_the_idp_was_down` — the sync fails with back-off while the IdP is unreachable
//!   and converges once it is back.
//! * `admin_api_enforces_permissions_sod_and_tenancy` — read / manage permissions, privileged
//!   role grants need a second administrator (maker-checker), no self-service on one's own
//!   identity, TOTP enrolment on privileged grants, administrative suspension, role mappings
//!   (SutraERP role names refused as targets, retired targets removed), tenant isolation.
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
use sutra_contracts::platform::auth::{NewSession, Principal, SessionPolicy, SessionStore};
use sutra_contracts::platform::identity::IdentityProvisioningPort;
use sutra_kernel::auth::{MfaLevel, Role};
use sutra_kernel::events::{EventEnvelope, NewEvent};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::jobs::run_once;
use sutra_platform::auth::session_pg::PgSessionStore;
use sutra_platform::identity::memory::MemoryIdentityProvider;
use sutra_platform::identity::sync::{IdpEventConsumer, IdpSyncWorker};
use sutra_platform::identity::{IdentityRuntime, IdentitySettings};
use uuid::Uuid;

const T_LIFE: &str = "0192f5a0-7c3e-7000-8000-0000000d1d01";
const T_DUP: &str = "0192f5a0-7c3e-7000-8000-0000000d1d02";
const T_RETRY: &str = "0192f5a0-7c3e-7000-8000-0000000d1d03";
const T_ADMIN: &str = "0192f5a0-7c3e-7000-8000-0000000d1d04";
const T_OTHER: &str = "0192f5a0-7c3e-7000-8000-0000000d1d05";

const ID: &str = "/api/v1/platform/identities";
const S: &str = "/api/v1/student";

/// The jobs run across tenants: keep this file's tests from syncing each other's identities.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Postgres-backed app (runtime role) in `tenant` with provisioning on (memory adapter).
async fn idp_app(tenant: &'static str) -> (Router, sqlx::PgPool) {
    let owner = owner_pool().await;
    let url = database_url();
    let (scheme, rest) = url.split_once("://").unwrap();
    let host = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    let runtime: &'static str =
        Box::leak(format!("{scheme}://{RUNTIME_USER}:{RUNTIME_PASSWORD}@{host}").into_boxed_str());
    let src: HashMap<&'static str, &'static str> = HashMap::from([
        ("SUTRA_ENV", "development"),
        ("SUTRA_DEV_AUTH_PASSWORD", sutra_api::app::SPEC_DEV_PASSWORD),
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

fn sessions(pool: &sqlx::PgPool) -> Arc<dyn SessionStore> {
    Arc::new(PgSessionStore::new(
        pool.clone(),
        SessionPolicy::new(1800, 28800).unwrap(),
    ))
}

/// Provisioning runtime over `idp` for `tenants` only (the jobs never touch other tests' data).
fn runtime(
    pool: &sqlx::PgPool,
    idp: Arc<MemoryIdentityProvider>,
    tenants: &[&str],
    auto: bool,
) -> IdentityRuntime {
    IdentityRuntime {
        port: idp,
        sources: sutra_api::idp_wiring::identity_sources(pool),
        sessions: sessions(pool),
        settings: IdentitySettings {
            auto_provision: auto,
            retry_base: Duration::from_secs(30),
            retry_max: Duration::from_secs(60),
            tenants: Some(tenants.iter().map(|t| t.parse().unwrap()).collect()),
            ..IdentitySettings::default()
        },
    }
}

async fn events(pool: &sqlx::PgPool, rt: &IdentityRuntime) {
    run_once(
        pool,
        &IdpEventConsumer {
            runtime: rt.clone(),
        },
    )
    .await
    .expect("idp-events");
}

async fn sync(pool: &sqlx::PgPool, rt: &IdentityRuntime) {
    run_once(
        pool,
        &IdpSyncWorker {
            runtime: rt.clone(),
        },
    )
    .await
    .expect("idp-sync");
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

async fn read(app: &Router, who: &Login, path: &str) -> (StatusCode, Value) {
    let (s, _, b) = send(app, get(who, path)).await;
    (s, b)
}

/// The link row as JSON (owner pool, system context).
async fn link(pool: &sqlx::PgPool, tenant: &str, subject: Uuid) -> Value {
    sutra_kernel::db_context::system(async {
        sqlx::query_scalar::<_, Value>(
            "SELECT to_jsonb(l) FROM platform.identity_link l \
             WHERE tenant_id = $1::uuid AND subject_id = $2",
        )
        .bind(tenant)
        .bind(subject)
        .fetch_one(pool)
        .await
        .expect("identity link")
    })
    .await
}

async fn audit_actions(pool: &sqlx::PgPool, link_id: &str) -> Vec<String> {
    sutra_kernel::db_context::system(async {
        sqlx::query_scalar(
            "SELECT action FROM platform.audit_log WHERE target_id = $1 ORDER BY occurred_at, id",
        )
        .bind(link_id)
        .fetch_all(pool)
        .await
        .unwrap()
    })
    .await
}

async fn exec(pool: &sqlx::PgPool, sql: &str, id: Uuid) {
    sutra_kernel::db_context::system(async {
        sqlx::query(sql).bind(id).execute(pool).await.unwrap();
    })
    .await;
}

/// Publishes a `student.status_changed` (to `active`) the way Domain C's lifecycle helper does.
async fn student_activated(
    pool: &sqlx::PgPool,
    st: &support::b3::SeededStudent,
    tenant: &str,
    inst: Uuid,
) {
    use sutra_contracts::student::events::{STUDENT_STATUS_CHANGED, STUDENT_STATUS_CHANGED_SCHEMA};
    let scope = ExecutionScope::institution(TenantId(tenant.parse().unwrap()), InstitutionId(inst));
    let env = EventEnvelope::new(
        &scope,
        NewEvent {
            source: sutra_contracts::student::events::SOURCE,
            event_type: STUDENT_STATUS_CHANGED,
            dataschema: STUDENT_STATUS_CHANGED_SCHEMA,
            subject: Some(st.student_id.to_string()),
            data: json!({
                "studentId": st.student_id, "studentContextId": st.context_id,
                "institutionId": inst, "scope": "context", "fromStatus": "pending_activation",
                "toStatus": "active", "causeEventType": "sutraerp.student.student.enrolled",
                "reason": null, "version": 2,
            }),
        },
    );
    sutra_kernel::db_context::system(async {
        sutra_persistence::outbox::enqueue(pool, &env)
            .await
            .unwrap();
    })
    .await;
}

async fn seeded(
    pool: &sqlx::PgPool,
    setup: &support::b3::AcademicSetup,
    name: &str,
    email: Option<&str>,
) -> support::b3::SeededStudent {
    let st = support::b3::seed_student(pool, setup, name, None).await;
    sutra_kernel::db_context::system(async {
        sqlx::query("UPDATE student.student SET primary_email = $2 WHERE id = $1")
            .bind(st.student_id)
            .bind(email)
            .execute(pool)
            .await
            .unwrap();
    })
    .await;
    st
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str()
        .unwrap_or_else(|| panic!("no {k} in {v}"))
        .to_owned()
}

#[tokio::test]
async fn student_lifecycle_drives_the_identity() {
    let _g = SERIAL.lock().await;
    let (app, pool) = idp_app(T_LIFE).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = support::b3::academic_setup(&app, &admin, T_LIFE, "IdP lifecycle").await;
    let email = format!("asha.{}@student.example.edu", Uuid::now_v7().simple());
    let st = seeded(&pool, &setup, "Asha", Some(&email)).await;
    let idp = Arc::new(MemoryIdentityProvider::new());
    let rt = runtime(&pool, idp.clone(), &[T_LIFE], true);

    // Activation event → identity created and queued, then provisioned.
    student_activated(&pool, &st, T_LIFE, setup.institution_id).await;
    events(&pool, &rt).await;
    let l = link(&pool, T_LIFE, st.student_id).await;
    assert_eq!(l["sync_status"], "pending");
    assert_eq!(l["desired_state"], "active");
    assert_eq!(l["username"], email.as_str());
    assert_eq!(
        l["pending_actions"],
        json!(["VERIFY_EMAIL", "UPDATE_PASSWORD"])
    );
    sync(&pool, &rt).await;
    let l = link(&pool, T_LIFE, st.student_id).await;
    assert_eq!(l["sync_status"], "synced", "{l}");
    let uid = s(&l, "idp_user_id");
    let u = idp.snapshot(&uid).unwrap();
    assert!(u.enabled);
    assert!(u.realm_roles.contains("student"));
    assert!(
        u.required_actions.contains("VERIFY_EMAIL")
            && u.required_actions.contains("UPDATE_PASSWORD")
    );
    let paths: Vec<String> = u.groups.iter().filter_map(|g| idp.group_path(g)).collect();
    assert_eq!(
        paths,
        vec![format!(
            "/sutra-tenant-{T_LIFE}/institution-{}",
            setup.institution_id
        )]
    );
    let subject_ref: Option<String> = sutra_kernel::db_context::system(async {
        sqlx::query_scalar("SELECT subject_ref FROM student.student WHERE id = $1")
            .bind(st.student_id)
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
    // Replaying the same event and syncing again changes nothing (idempotent).
    events(&pool, &rt).await;
    sync(&pool, &rt).await;
    assert_eq!(idp.user_count(), 1);

    // A live SutraERP session of the student.
    let store = sessions(&pool);
    let scope = ExecutionScope::tenant(TenantId(T_LIFE.parse().unwrap()));
    let issued = sutra_kernel::db_context::system(store.create(NewSession {
        principal: Principal {
            subject: uid.clone(),
            display_name: "Asha".into(),
            login_id: email.clone(),
            roles: vec![Role::Student],
            scope,
        },
        mfa_level: MfaLevel::None,
        amr: vec!["pwd".into()],
        idp_sid: Some("kc-sid".into()),
        not_after: None,
    }))
    .await
    .unwrap();

    // ERP suspension → account disabled, IdP + SutraERP sessions ended.
    let sid = st.student_id;
    let (code, stu) = read(&app, &admin, &format!("{S}/students/{sid}")).await;
    assert_eq!(code, StatusCode::OK, "{stu}");
    let ver = stu["currentContext"]["version"].as_i64().unwrap();
    let (code, body) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{S}/students/{sid}/suspend"),
            json!({"reason": "disciplinary hearing", "category": "disciplinary"}),
            Some(ver),
        ),
    )
    .await
    .into_status_body();
    assert_eq!(code, StatusCode::OK, "{body}");
    events(&pool, &rt).await;
    let l = link(&pool, T_LIFE, st.student_id).await;
    assert_eq!(l["desired_state"], "suspended");
    sync(&pool, &rt).await;
    let u = idp.snapshot(&uid).unwrap();
    assert!(!u.enabled, "suspended student cannot sign in");
    assert!(u.logouts >= 1, "IdP sessions ended");
    let gone = sutra_kernel::db_context::system(store.get(&issued.token))
        .await
        .unwrap();
    assert!(gone.is_none(), "SutraERP session revoked");

    // Restoration → enabled again.
    let ver = body["currentContext"]["version"].as_i64().unwrap();
    let (code, body) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{S}/students/{sid}/restore"),
            json!({"reason": "hearing closed"}),
            Some(ver),
        ),
    )
    .await
    .into_status_body();
    assert_eq!(code, StatusCode::OK, "{body}");
    events(&pool, &rt).await;
    sync(&pool, &rt).await;
    assert!(idp.snapshot(&uid).unwrap().enabled);
    let l = link(&pool, T_LIFE, st.student_id).await;
    let actions = audit_actions(&pool, &s(&l, "id")).await;
    for a in [
        "platform.identity.link.created",
        "platform.identity.provisioned",
        "platform.identity.disabled",
        "platform.identity.enabled",
    ] {
        assert!(actions.iter().any(|x| x == a), "{a} audited: {actions:?}");
    }
}

/// `(status, body)` of a [`send`] result.
trait StatusBody {
    fn into_status_body(self) -> (StatusCode, Value);
}

impl StatusBody for (StatusCode, axum::http::HeaderMap, Value) {
    fn into_status_body(self) -> (StatusCode, Value) {
        (self.0, self.2)
    }
}

#[tokio::test]
async fn duplicate_identities_are_prevented() {
    let _g = SERIAL.lock().await;
    let (app, pool) = idp_app(T_DUP).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = support::b3::academic_setup(&app, &admin, T_DUP, "IdP duplicates").await;
    let tag = Uuid::now_v7().simple().to_string();
    let email = format!("ravi.{tag}@student.example.edu");
    let a = seeded(&pool, &setup, "Ravi", Some(&email)).await;
    let b = seeded(&pool, &setup, "Ravi B", Some(&email.to_uppercase())).await;
    let idp = Arc::new(MemoryIdentityProvider::new());
    let rt = runtime(&pool, idp.clone(), &[T_DUP], false);

    let body = json!({"subjectType": "student", "subjectId": a.student_id});
    let (code, first) = call(
        &app,
        &admin,
        "POST",
        &format!("{ID}/provision"),
        body.clone(),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{first}");
    let (code, again) = call(&app, &admin, "POST", &format!("{ID}/provision"), body).await;
    assert_eq!(code, StatusCode::OK, "{again}");
    assert_eq!(first["id"], again["id"], "one identity per subject");
    // Same e-mail (case-insensitive) on another student → refused.
    let (code, dup) = call(
        &app,
        &admin,
        "POST",
        &format!("{ID}/provision"),
        json!({"subjectType": "student", "subjectId": b.student_id}),
    )
    .await;
    assert_eq!(code, StatusCode::CONFLICT, "{dup}");
    assert!(
        dup.to_string().contains("duplicate identity prevented"),
        "{dup}"
    );

    // The person already has an IdP account (same e-mail): adopted, not duplicated.
    let existing = idp.seed_user(&format!("legacy-{tag}"), Some(&email));
    sync(&pool, &rt).await;
    let l = link(&pool, T_DUP, a.student_id).await;
    assert_eq!(l["sync_status"], "synced", "{l}");
    assert_eq!(l["idp_user_id"], existing.as_str());
    assert_eq!(idp.user_count(), 1);
    let actions = audit_actions(&pool, &s(&l, "id")).await;
    assert!(
        actions.iter().any(|x| x == "platform.identity.adopted"),
        "{actions:?}"
    );

    // An account that belongs to another tenant is never adopted.
    let c = seeded(
        &pool,
        &setup,
        "Chitra",
        Some(&format!("chitra.{tag}@student.example.edu")),
    )
    .await;
    let foreign = idp.seed_user(&format!("chitra.{tag}@student.example.edu"), None);
    let g = idp
        .ensure_scope_group(&sutra_contracts::platform::identity::ScopeGroup {
            tenant_id: T_OTHER.parse().unwrap(),
            institution_id: None,
        })
        .await
        .unwrap();
    idp.join_group(&foreign, &g).await.unwrap();
    let (code, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{ID}/provision"),
        json!({"subjectType": "student", "subjectId": c.student_id}),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED);
    sync(&pool, &rt).await;
    let l = link(&pool, T_DUP, c.student_id).await;
    assert_eq!(l["sync_status"], "conflict", "{l}");
    assert!(s(&l, "last_error").contains("another tenant"), "{l}");
    assert!(l["idp_user_id"].is_null());
    // The database itself refuses a second link to the same IdP account (any tenant).
    let err = sutra_kernel::db_context::system(async {
        sqlx::query(
            "UPDATE platform.identity_link SET idp_user_id = $2 WHERE tenant_id = $1::uuid AND subject_id = $3",
        )
        .bind(T_DUP)
        .bind(&existing)
        .bind(c.student_id)
        .execute(&pool)
        .await
    })
    .await;
    assert!(err.is_err(), "unique (idp, idp_user_id)");
}

#[tokio::test]
async fn retry_after_the_idp_was_down() {
    let _g = SERIAL.lock().await;
    let (app, pool) = idp_app(T_RETRY).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = support::b3::academic_setup(&app, &admin, T_RETRY, "IdP retry").await;
    let st = seeded(&pool, &setup, "Meena", None).await;
    let idp = Arc::new(MemoryIdentityProvider::new());
    let rt = runtime(&pool, idp.clone(), &[T_RETRY], false);
    let (code, v) = call(
        &app,
        &admin,
        "POST",
        &format!("{ID}/provision"),
        json!({"subjectType": "student", "subjectId": st.student_id}),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{v}");
    assert_eq!(
        v["username"].as_str().unwrap(),
        format!(
            "{}.{}",
            st.student_number.to_lowercase(),
            &T_RETRY.replace('-', "")[20..]
        )
    );

    idp.set_down(true);
    sync(&pool, &rt).await;
    let l = link(&pool, T_RETRY, st.student_id).await;
    assert_eq!(l["sync_status"], "failed", "{l}");
    assert_eq!(l["attempts"], 1);
    assert!(s(&l, "last_error").contains("unreachable"));
    // Backed off: an immediate tick does not retry.
    idp.set_down(false);
    sync(&pool, &rt).await;
    assert_eq!(link(&pool, T_RETRY, st.student_id).await["attempts"], 1);
    assert_eq!(idp.user_count(), 0);
    // Due again (as after the back-off): converges, exactly one account.
    exec(
        &pool,
        "UPDATE platform.identity_link SET next_attempt_at = now() WHERE subject_id = $1",
        st.student_id,
    )
    .await;
    sync(&pool, &rt).await;
    let l = link(&pool, T_RETRY, st.student_id).await;
    assert_eq!(l["sync_status"], "synced", "{l}");
    assert_eq!(l["attempts"], 0);
    assert_eq!(idp.user_count(), 1);
    let actions = audit_actions(&pool, &s(&l, "id")).await;
    assert!(actions.iter().any(|x| x == "platform.identity.sync.failed"));
    assert!(actions.iter().any(|x| x == "platform.identity.provisioned"));
}

#[tokio::test]
async fn admin_api_enforces_permissions_sod_and_tenancy() {
    let _g = SERIAL.lock().await;
    let (app, pool) = idp_app(T_ADMIN).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let dean = login(&app, "dean@sutra.dev").await;
    let faculty = login(&app, "meera.shah@sutra.dev").await;
    let idp = Arc::new(MemoryIdentityProvider::new());
    let rt = runtime(&pool, idp.clone(), &[T_ADMIN], false);

    // Permissions.
    assert_eq!(read(&app, &faculty, ID).await.0, StatusCode::FORBIDDEN);
    let tag = Uuid::now_v7().simple().to_string();
    let new_admin = format!("ops.{tag}@college.example.edu");
    let body = json!({
        "email": new_admin, "givenName": "Ops", "familyName": "Admin",
        "roles": ["administrator"], "reason": "second platform administrator",
    });
    assert_eq!(
        call(
            &app,
            &faculty,
            "POST",
            &format!("{ID}/admins"),
            body.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    // No identity for oneself.
    let (code, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{ID}/admins"),
        json!({"email": "admin@sutra.dev", "givenName": "Me", "roles": ["administrator"], "reason": "x"}),
    )
    .await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    let (code, created) = call(&app, &admin, "POST", &format!("{ID}/admins"), body).await;
    assert_eq!(code, StatusCode::CREATED, "{created}");
    let link_id = s(&created["identity"], "id");
    let req_id = s(&created["roleRequest"], "id");
    assert_eq!(created["roleRequest"]["status"], "pending");
    assert_eq!(created["roleRequest"]["privileged"], true);
    assert_eq!(
        created["identity"]["roles"],
        json!([]),
        "nothing granted before the decision"
    );

    // Maker-checker: the requester cannot decide; another administrator can.
    let decision = format!("/api/v1/platform/identity-role-requests/{req_id}/decision");
    let (code, v) = call(
        &app,
        &admin,
        "POST",
        &decision,
        json!({"decision": "approve", "reason": "ok"}),
    )
    .await;
    assert_eq!(code, StatusCode::FORBIDDEN, "{v}");
    assert!(v.to_string().contains("separation of duties"), "{v}");
    assert_eq!(
        call(
            &app,
            &faculty,
            "POST",
            &decision,
            json!({"decision": "approve", "reason": "ok"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (code, v) = call(
        &app,
        &dean,
        "POST",
        &decision,
        json!({"decision": "approve", "reason": "verified"}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "approved");
    let (code, v) = call(
        &app,
        &dean,
        "POST",
        &decision,
        json!({"decision": "approve", "reason": "again"}),
    )
    .await;
    assert_eq!(code, StatusCode::CONFLICT, "{v}");
    let (_, ident) = read(&app, &admin, &format!("{ID}/{link_id}")).await;
    assert_eq!(ident["roles"], json!(["administrator"]));
    assert!(
        ident["pendingActions"]
            .as_array()
            .unwrap()
            .contains(&json!("CONFIGURE_TOTP")),
        "{ident}"
    );
    sync(&pool, &rt).await;
    let (_, ident) = read(&app, &admin, &format!("{ID}/{link_id}")).await;
    assert_eq!(ident["syncStatus"], "synced", "{ident}");
    let uid = s(&ident, "idpUserId");
    let u = idp.snapshot(&uid).unwrap();
    assert!(u.realm_roles.contains("administrator"));
    assert!(
        u.required_actions.contains("CONFIGURE_TOTP"),
        "privileged role → TOTP enrolment"
    );

    // Password recovery trigger.
    let (code, v) = call(
        &app,
        &admin,
        "POST",
        &format!("{ID}/{link_id}/required-actions"),
        json!({"actions": ["updatePassword"], "sendEmail": true}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{v}");
    sync(&pool, &rt).await;
    let u = idp.snapshot(&uid).unwrap();
    assert_eq!(u.action_emails, vec![vec!["UPDATE_PASSWORD".to_owned()]]);

    // Administrative suspension / re-enabling (never of one's own identity).
    let (code, v) = call(
        &app,
        &dean,
        "POST",
        &format!("{ID}/{link_id}/suspend"),
        json!({"reason": "left the project"}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{v}");
    assert_eq!(v["desiredState"], "suspended");
    sync(&pool, &rt).await;
    assert!(!idp.snapshot(&uid).unwrap().enabled);
    assert_eq!(
        call(
            &app,
            &dean,
            "POST",
            &format!("{ID}/{link_id}/suspend"),
            json!({"reason": "again"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (code, _) = call(
        &app,
        &dean,
        "POST",
        &format!("{ID}/{link_id}/reactivate"),
        json!({"reason": "back"}),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    sync(&pool, &rt).await;
    assert!(idp.snapshot(&uid).unwrap().enabled);
    // The new administrator (once signed in) may not act on their own identity.
    let own = sutra_kernel::db_context::system(async {
        let store = sessions(&pool);
        store
            .create(NewSession {
                principal: Principal {
                    subject: uid.clone(),
                    display_name: "Ops".into(),
                    login_id: new_admin.clone(),
                    roles: vec![Role::Administrator],
                    scope: ExecutionScope::tenant(TenantId(T_ADMIN.parse().unwrap())),
                },
                mfa_level: MfaLevel::Mfa,
                amr: vec!["pwd".into(), "otp".into()],
                idp_sid: None,
                not_after: None,
            })
            .await
            .unwrap()
    })
    .await;
    let me = Login {
        cookie: format!("__Host-sutra_session={}", own.token),
        csrf: own.session.csrf_token.clone(),
    };
    let (code, v) = call(
        &app,
        &me,
        "POST",
        &format!("{ID}/{link_id}/suspend"),
        json!({"reason": "self"}),
    )
    .await;
    assert_eq!(code, StatusCode::FORBIDDEN, "{v}");
    let (code, _) = call(
        &app,
        &me,
        "POST",
        &format!("{ID}/{link_id}/role-requests"),
        json!({"grant": ["registrar"], "reason": "self-grant"}),
    )
    .await;
    assert_eq!(code, StatusCode::FORBIDDEN);

    // Revoking is immediate and ends sessions at the next sync.
    let (code, v) = call(
        &app,
        &dean,
        "POST",
        &format!("{ID}/{link_id}/role-requests"),
        json!({"revoke": ["administrator"], "reason": "rotation"}),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{v}");
    assert_eq!(v["status"], "applied");
    sync(&pool, &rt).await;
    assert!(!idp
        .snapshot(&uid)
        .unwrap()
        .realm_roles
        .contains("administrator"));
    let gone = sutra_kernel::db_context::system(sessions(&pool).get(&own.token))
        .await
        .unwrap();
    assert!(gone.is_none(), "role removal ends the session now");

    // Role mappings: SutraERP role names are not targets; retired targets are removed.
    let maps = "/api/v1/platform/identity-role-mappings";
    let (code, _) = call(
        &app,
        &dean,
        "POST",
        maps,
        json!({"erpRole": "student", "targetKind": "realm_role", "target": "administrator"}),
    )
    .await;
    assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
    let (code, _) = call(&app, &dean, "POST", maps, json!({"erpRole": "faculty", "targetKind": "group", "target": format!("/sutra-tenant-{T_OTHER}")})).await;
    assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        call(
            &app,
            &faculty,
            "POST",
            maps,
            json!({"erpRole": "registrar", "targetKind": "realm_role", "target": "lms-staff"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (code, _) = call(
        &app,
        &dean,
        "POST",
        &format!("{ID}/{link_id}/role-requests"),
        json!({"grant": ["faculty"], "reason": "teaches"}),
    )
    .await;
    assert_eq!(
        code,
        StatusCode::CREATED,
        "non-privileged grant applies at once"
    );
    let (code, m) = call(
        &app,
        &dean,
        "POST",
        maps,
        json!({"erpRole": "faculty", "targetKind": "realm_role", "target": "lms-staff"}),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{m}");
    sync(&pool, &rt).await;
    let u = idp.snapshot(&uid).unwrap();
    assert!(
        u.realm_roles.contains("faculty") && u.realm_roles.contains("lms-staff"),
        "{:?}",
        u.realm_roles
    );
    let (code, _) = call(
        &app,
        &dean,
        "DELETE",
        &format!("{maps}/{}", s(&m, "id")),
        json!({}),
    )
    .await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    sync(&pool, &rt).await;
    assert!(!idp
        .snapshot(&uid)
        .unwrap()
        .realm_roles
        .contains("lms-staff"));

    // Tenant isolation: another tenant's administrator sees nothing of it.
    let (other_app, _) = idp_app(T_OTHER).await;
    let other = login(&other_app, "admin@sutra.dev").await;
    assert_eq!(
        read(&other_app, &other, &format!("{ID}/{link_id}")).await.0,
        StatusCode::NOT_FOUND
    );
    let (code, page) = read(&other_app, &other, ID).await;
    assert_eq!(code, StatusCode::OK);
    assert!(!page.to_string().contains(&link_id));
    let actions = audit_actions(&pool, &link_id).await;
    for a in [
        "platform.identity.admin.created",
        "platform.identity.role_request.created",
        "platform.identity.role_request.approved",
        "platform.identity.required_actions.requested",
        "platform.identity.suspend",
        "platform.identity.reactivate",
        "platform.identity.roles.changed",
        "platform.identity.roles.synced",
    ] {
        assert!(actions.iter().any(|x| x == a), "{a} audited: {actions:?}");
    }
}
