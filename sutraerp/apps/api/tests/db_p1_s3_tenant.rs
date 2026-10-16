//! Part 1 Stage 3 — tenant isolation (DEF-X-003 / SEC-9D-004, DEF-C-010, DEF-I-006) on real
//! PostgreSQL, against the row-level security of migration `20261230000100_platform_tenant_rls.sql`
//! and the per-task context of `sutra_kernel::db_context` + `sutra_persistence::pool`.
//!
//! * `rls_catalog_covers_every_tenant_table` — every table with a `tenant_id` column has RLS
//!   enabled + forced and only the `sutra_tenant_isolation` policy (`platform.tenant_rls_gaps`
//!   empty); the runtime login role is neither superuser nor BYPASSRLS.
//! * `id_only_sql_as_runtime_role_cannot_touch_another_tenant` — a tenant-B database session
//!   running by-id SQL with **no tenant predicate** (the DEF-C-011 / E-008 / … bug class) sees 0
//!   rows of tenant A's student, document, charge, term result, staff record and payroll run, and
//!   its UPDATE / DELETE affect 0 rows; a forged `tenant_id` on INSERT and moving a row to another
//!   tenant are refused (42501); no context at all sees nothing (fails closed).
//! * `api_session_of_one_tenant_cannot_reach_another_tenant` — a signed-in administrator of tenant
//!   B gets 403/404 for each of those six records of tenant A (tenant A's own administrator reads
//!   its student: positive control) and cannot write to them; faculty of tenant A get 403 on
//!   unpublished results (DEF-I-006).
//! * `every_tenant_table_hides_foreign_rows_from_the_runtime_role` — sweeps every tenant table as
//!   the runtime role: an unknown tenant and no context see 0 rows; a tenant-A context sees no row
//!   of any other tenant.
//! * `pooled_connections_never_carry_a_tenant_into_the_next_task` — the pool writes the context of
//!   the acquiring task and clears it on release: after a tenant-A task the same physical
//!   connection carries nothing; concurrent tenant-A / tenant-B tasks on one pool each see only
//!   their own tenant; a task spawned without the context sees nothing.
//! * `pinned_campus_session_cannot_reach_another_campus` — a session pinned to institution 1 of a
//!   tenant does not see or change institution 2's rows and cannot move a row there (DEF-C-010).
//!
//! Tenant A's records are seeded as the schema owner (superuser in CI) with
//! `session_replication_role = replica`, i.e. without foreign-key triggers, so one row per table can
//! be created without the whole domain setup; the assertions are about row visibility, which does
//! not depend on the referenced rows.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use serde_json::json;
use sqlx::{Connection, PgConnection, PgPool};
use support::*;
use sutra_kernel::db_context;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use uuid::Uuid;

/// Tenant A's seeded records.
#[derive(Clone, Copy, Debug)]
struct Seeded {
    tenant: Uuid,
    institution: Uuid,
    other_institution: Uuid,
    student: Uuid,
    document: Uuid,
    charge: Uuid,
    other_campus_charge: Uuid,
    term_result: Uuid,
    staff: Uuid,
    payroll_run: Uuid,
}

impl Seeded {
    /// (table, id) of the six records every cross-tenant check covers.
    fn targets(&self) -> [(&'static str, Uuid); 6] {
        [
            ("student.student", self.student),
            ("student.student_document", self.document),
            ("finance.charge", self.charge),
            ("exams.term_result", self.term_result),
            ("people.staff_member", self.staff),
            ("people.payroll_run", self.payroll_run),
        ]
    }
}

fn suffix() -> String {
    Uuid::now_v7().simple().to_string()[20..].to_uppercase()
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

async fn seed_tenant(owner: &PgPool, tenant: Uuid) -> Seeded {
    let (institution, other_institution) = (Uuid::now_v7(), Uuid::now_v7());
    let sfx = suffix();
    let mut tx = owner.begin().await.unwrap();
    // Seeding only: no FK triggers (see the module docs); system scope in case the owner is not a
    // superuser (RLS is FORCEd for the owner too).
    sqlx::query("SET LOCAL session_replication_role = replica")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("SELECT set_config('sutra.rls_scope', 'system', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    let student: Uuid = sqlx::query_scalar(
        "INSERT INTO student.student (tenant_id, student_number, given_name, family_name, status) \
         VALUES ($1, $2, 'Isha', 'Victim', 'active') RETURNING id",
    )
    .bind(tenant)
    .bind(format!("RLS-{sfx}"))
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO student.student_context (tenant_id, student_id, institution_id, programme_id, \
         curriculum_version_id, admission_term_id, admission_year_id, current_term_no, \
         enrolment_number, batch_code, section_code, enrolment_type, enrolled_on, status) \
         VALUES ($1, $2, $3, gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), \
         gen_random_uuid(), 1, $4, '2026', 'A', 'regular', DATE '2026-07-01', 'active')",
    )
    .bind(tenant)
    .bind(student)
    .bind(institution)
    .bind(format!("EN-RLS-{sfx}"))
    .execute(&mut *tx)
    .await
    .unwrap();
    let document: Uuid = sqlx::query_scalar(
        "INSERT INTO student.student_document (tenant_id, student_id, institution_id, \
         document_type, title) VALUES ($1, $2, $3, 'transcript', 'Transcript of records') \
         RETURNING id",
    )
    .bind(tenant)
    .bind(student)
    .bind(institution)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let mut charges = Vec::new();
    for (inst, n) in [(institution, 1), (other_institution, 2)] {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO finance.charge (tenant_id, institution_id, student_account_id, student_id, \
             charge_number, description, total_amount, source_type, status) \
             VALUES ($1, $2, gen_random_uuid(), $3, $4, 'Tuition fee', 1500.00, 'manual', \
             'published') RETURNING id",
        )
        .bind(tenant)
        .bind(inst)
        .bind(student)
        .bind(format!("CH-RLS-{sfx}-{n}"))
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        charges.push(id);
    }
    // Unpublished (computed) result: confidential to the examination office.
    let term_result: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.term_result (tenant_id, institution_id, session_id, registration_id, \
         student_id, student_context_id, academic_term_id, student_number, display_name, \
         grading_scheme_id, outcome, status) VALUES ($1, $2, gen_random_uuid(), \
         gen_random_uuid(), $3, gen_random_uuid(), gen_random_uuid(), $4, 'Isha Victim', \
         gen_random_uuid(), 'pass', 'computed') RETURNING id",
    )
    .bind(tenant)
    .bind(institution)
    .bind(student)
    .bind(format!("RLS-{sfx}"))
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let staff: Uuid = sqlx::query_scalar(
        "INSERT INTO people.staff_member (tenant_id, institution_id, staff_number, given_name, \
         display_name, staff_category, status) VALUES ($1, $2, 'RLS-000001', 'Kavya', \
         'Kavya Victim', 'teaching', 'active') RETURNING id",
    )
    .bind(tenant)
    .bind(institution)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let payroll_run: Uuid = sqlx::query_scalar(
        "INSERT INTO people.payroll_run (tenant_id, institution_id, period_id, number, kind) \
         VALUES ($1, $2, gen_random_uuid(), 'PR-000001', 'regular') RETURNING id",
    )
    .bind(tenant)
    .bind(institution)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    Seeded {
        tenant,
        institution,
        other_institution,
        student,
        document,
        charge: charges[0],
        other_campus_charge: charges[1],
        term_result,
        staff,
        payroll_run,
    }
}

/// URL of the runtime login role (member of `sutra_app`, what the API serves as).
fn runtime_url() -> String {
    let url = database_url();
    let (scheme, rest) = url.split_once("://").expect("postgres URL");
    let host = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    format!("{scheme}://{RUNTIME_USER}:{RUNTIME_PASSWORD}@{host}")
}

/// A single runtime-role connection whose context the test sets explicitly (independent of the
/// pool hooks: this exercises the database policy itself).
async fn runtime_conn() -> PgConnection {
    PgConnection::connect(&runtime_url()).await.unwrap()
}

/// The API's own pool implementation, connected as the runtime role.
fn runtime_pool(max: u32) -> PgPool {
    sutra_persistence::pool::connect_lazy(&sutra_persistence::pool::PoolSettings {
        url: runtime_url(),
        max_connections: max,
        min_connections: 0,
        acquire_timeout: std::time::Duration::from_secs(10),
        application_name: "sutra-api-db-tests-s3-tenant".into(),
    })
    .unwrap()
}

async fn set_ctx(c: &mut PgConnection, tenant: Option<Uuid>, inst: Option<Uuid>, scope: &str) {
    sqlx::query(
        "SELECT set_config('sutra.tenant_id', $1, false), \
         set_config('sutra.institution_id', $2, false), set_config('sutra.rls_scope', $3, false)",
    )
    .bind(tenant.map(|t| t.to_string()).unwrap_or_default())
    .bind(inst.map(|i| i.to_string()).unwrap_or_default())
    .bind(scope)
    .execute(&mut *c)
    .await
    .unwrap();
}

async fn count_by_id(c: &mut PgConnection, table: &str, id: Uuid) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE id = $1"))
        .bind(id)
        .fetch_one(&mut *c)
        .await
        .unwrap()
}

fn sqlstate(e: &sqlx::Error) -> Option<String> {
    e.as_database_error()
        .and_then(|d| d.code())
        .map(|c| c.into_owned())
}

/// Rows affected, or 0 when the statement is refused outright with insufficient_privilege.
async fn affected_or_refused(c: &mut PgConnection, sql: &str, id: Uuid) -> u64 {
    match sqlx::query(sql).bind(id).execute(&mut *c).await {
        Ok(r) => r.rows_affected(),
        Err(e) => {
            assert_eq!(sqlstate(&e).as_deref(), Some("42501"), "{sql}: {e}");
            0
        }
    }
}

#[tokio::test]
async fn rls_catalog_covers_every_tenant_table() {
    let owner = owner_pool().await;
    let gaps: Vec<(String, String)> = sqlx::query_as(
        "SELECT schema_name, table_name FROM platform.tenant_rls_gaps ORDER BY 1, 2",
    )
    .fetch_all(&owner)
    .await
    .unwrap();
    assert!(
        gaps.is_empty(),
        "tenant tables without forced RLS + sutra_tenant_isolation only: {gaps:?}"
    );
    let (tables, policies): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM pg_class c JOIN pg_namespace ns ON ns.oid = c.relnamespace \
                  JOIN pg_attribute a ON a.attrelid = c.oid AND a.attname = 'tenant_id' \
                   AND a.attnum > 0 AND NOT a.attisdropped \
                 WHERE c.relkind IN ('r', 'p') AND c.relrowsecurity AND c.relforcerowsecurity \
                   AND ns.nspname NOT IN ('pg_catalog', 'information_schema') \
                   AND ns.nspname NOT LIKE 'pg\\_%'), \
                (SELECT count(*) FROM pg_policy WHERE polname = 'sutra_tenant_isolation')",
    )
    .fetch_one(&owner)
    .await
    .unwrap();
    assert!(
        tables >= 300,
        "expected every tenant table (~386) to be covered, got {tables}"
    );
    assert_eq!(tables, policies, "one isolation policy per tenant table");
    let role = sutra_persistence::pool::role_security(&runtime_pool(1))
        .await
        .unwrap();
    assert_eq!(role.role, RUNTIME_USER);
    assert!(
        !role.bypasses_rls(),
        "the runtime role must be subject to RLS: {role:?}"
    );
}

/// S3-int capacity regression (quality 38856685913): the isolation predicate must read the
/// session settings once per statement (InitPlan), never once per row. Every policy calls the
/// settings functions only inside a scalar sub-select.
#[tokio::test]
async fn isolation_policies_read_the_session_context_once_per_statement() {
    let owner = owner_pool().await;
    let per_row: Vec<(String,)> = sqlx::query_as(
        "SELECT p.polrelid::regclass::text FROM pg_policy p \
          WHERE p.polname = 'sutra_tenant_isolation' \
            AND (regexp_replace(pg_get_expr(p.polqual, p.polrelid), \
                   '\\(\\s*SELECT\\s+(platform\\.)?rls_(tenant|institution|system)\\(\\)[^)]*\\)', '', 'g') \
                   ~ 'rls_(tenant|institution|system)\\(\\)' \
              OR regexp_replace(pg_get_expr(p.polwithcheck, p.polrelid), \
                   '\\(\\s*SELECT\\s+(platform\\.)?rls_(tenant|institution|system)\\(\\)[^)]*\\)', '', 'g') \
                   ~ 'rls_(tenant|institution|system)\\(\\)') \
          ORDER BY 1",
    )
    .fetch_all(&owner)
    .await
    .unwrap();
    assert!(
        per_row.is_empty(),
        "policies evaluating the RLS context per row: {per_row:?}"
    );
}

#[tokio::test]
async fn id_only_sql_as_runtime_role_cannot_touch_another_tenant() {
    let owner = owner_pool().await;
    let a = seed_tenant(&owner, Uuid::now_v7()).await;
    let tenant_b = Uuid::now_v7();
    let mut c = runtime_conn().await;

    // Positive control: tenant A's own context sees each record by id.
    set_ctx(&mut c, Some(a.tenant), None, "tenant").await;
    for (table, id) in a.targets() {
        assert_eq!(
            count_by_id(&mut c, table, id).await,
            1,
            "{table} own tenant"
        );
    }

    // Tenant B: by-id SQL without any tenant predicate.
    set_ctx(&mut c, Some(tenant_b), None, "tenant").await;
    for (table, id) in a.targets() {
        assert_eq!(
            count_by_id(&mut c, table, id).await,
            0,
            "{table}: tenant B must not read tenant A's row by id"
        );
        let upd = format!("UPDATE {table} SET updated_at = now() WHERE id = $1");
        assert_eq!(
            affected_or_refused(&mut c, &upd, id).await,
            0,
            "{table}: tenant B must not update tenant A's row"
        );
        let del = format!("DELETE FROM {table} WHERE id = $1");
        assert_eq!(
            affected_or_refused(&mut c, &del, id).await,
            0,
            "{table}: tenant B must not delete tenant A's row"
        );
    }
    // Forging tenant A's id on a write is refused by WITH CHECK.
    let forged = sqlx::query(
        "INSERT INTO student.student (tenant_id, student_number, given_name, status) \
         VALUES ($1, $2, 'Forged', 'active')",
    )
    .bind(a.tenant)
    .bind(format!("FORGED-{}", suffix()))
    .execute(&mut c)
    .await
    .expect_err("insert into another tenant must be refused");
    assert_eq!(sqlstate(&forged).as_deref(), Some("42501"), "{forged}");

    // Tenant A cannot move its row to tenant B either.
    set_ctx(&mut c, Some(a.tenant), None, "tenant").await;
    let moved = sqlx::query("UPDATE student.student SET tenant_id = $2 WHERE id = $1")
        .bind(a.student)
        .bind(tenant_b)
        .execute(&mut c)
        .await
        .expect_err("moving a row to another tenant must be refused");
    assert_eq!(sqlstate(&moved).as_deref(), Some("42501"), "{moved}");

    // No context at all (a code path that lost it): nothing visible, nothing writable.
    set_ctx(&mut c, None, None, "").await;
    for (table, id) in a.targets() {
        assert_eq!(
            count_by_id(&mut c, table, id).await,
            0,
            "{table} no context"
        );
        let upd = format!("UPDATE {table} SET updated_at = now() WHERE id = $1");
        assert_eq!(affected_or_refused(&mut c, &upd, id).await, 0, "{table}");
    }
    // A malformed scope value is not "system".
    set_ctx(&mut c, None, None, "SYSTEM ").await;
    assert_eq!(count_by_id(&mut c, "student.student", a.student).await, 0);

    // The rows are intact (owner view).
    let intact: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM student.student WHERE id = $1 AND tenant_id = $2) \
              + (SELECT count(*) FROM exams.term_result WHERE id = $3 AND status = 'computed') \
              + (SELECT count(*) FROM people.payroll_run WHERE id = $4)",
    )
    .bind(a.student)
    .bind(a.tenant)
    .bind(a.term_result)
    .bind(a.payroll_run)
    .fetch_one(&owner)
    .await
    .unwrap();
    assert_eq!(intact, 3);
}

#[tokio::test]
async fn api_session_of_one_tenant_cannot_reach_another_tenant() {
    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let (app_a, owner) = app(leak(tenant_a.to_string())).await;
    let (app_b, _) = app(leak(tenant_b.to_string())).await;
    let a = seed_tenant(&owner, tenant_a).await;
    let admin_a = login(&app_a, "admin@sutra.dev").await;
    let admin_b = login(&app_b, "admin@sutra.dev").await;

    // Positive control: tenant A's administrator reads tenant A's student.
    let path_student = format!("/api/v1/student/students/{}", a.student);
    let (s, _, body) = send(&app_a, get(&admin_a, &path_student)).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["id"], json!(a.student.to_string()));

    let reads = [
        path_student.clone(),
        format!("/api/v1/finance/charges/{}", a.charge),
        format!("/api/v1/exams/term-results/{}", a.term_result),
        format!("/api/v1/people/staff/{}", a.staff),
        format!("/api/v1/people/payroll-runs/{}", a.payroll_run),
    ];
    for path in &reads {
        let (s, _, body) = send(&app_b, get(&admin_b, path)).await;
        assert!(
            s == StatusCode::NOT_FOUND || s == StatusCode::FORBIDDEN,
            "tenant B admin GET {path} -> {s} {body}"
        );
    }
    // The document list of tenant A's student never shows its documents to tenant B.
    let docs = format!("/api/v1/student/students/{}/documents", a.student);
    let (s, _, body) = send(&app_b, get(&admin_b, &docs)).await;
    assert!(!s.is_success() || !body.to_string().contains(&a.document.to_string()));
    assert_ne!(s, StatusCode::INTERNAL_SERVER_ERROR, "{body}");

    // Writes from tenant B against tenant A's records are refused and change nothing.
    let (s, _, body) = send(
        &app_b,
        write(
            &admin_b,
            "POST",
            &format!("/api/v1/exams/term-results/{}/withhold", a.term_result),
            json!({"reason": "cross-tenant attempt"}),
            Some(1),
        ),
    )
    .await;
    assert!(!s.is_success(), "tenant B withhold -> {s} {body}");
    let (s, _, body) = send(
        &app_b,
        write(
            &admin_b,
            "POST",
            &docs,
            json!({"documentType": "transcript", "title": "Planted by tenant B"}),
            None,
        ),
    )
    .await;
    assert!(!s.is_success(), "tenant B document upload -> {s} {body}");
    let (status, docs_n): (String, i64) = sqlx::query_as(
        "SELECT (SELECT status FROM exams.term_result WHERE id = $1), \
                (SELECT count(*) FROM student.student_document WHERE student_id = $2)",
    )
    .bind(a.term_result)
    .bind(a.student)
    .fetch_one(&owner)
    .await
    .unwrap();
    assert_eq!((status.as_str(), docs_n), ("computed", 1));

    // DEF-I-006: faculty (exam.read only) cannot read unpublished results of their own tenant.
    let meera = login(&app_a, "meera.shah@sutra.dev").await;
    let tr = format!("/api/v1/exams/term-results/{}", a.term_result);
    let (s, _, body) = send(&app_a, get(&meera, &tr)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "faculty GET result: {body}");
    let list = format!("/api/v1/exams/exam-sessions/{}/results", Uuid::now_v7());
    let (s, _, body) = send(&app_a, get(&meera, &list)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "faculty list results: {body}");
    let (s, _, body) = send(&app_a, get(&admin_a, &tr)).await;
    assert_ne!(
        s,
        StatusCode::FORBIDDEN,
        "the examination office keeps access: {body}"
    );
}

#[tokio::test]
async fn every_tenant_table_hides_foreign_rows_from_the_runtime_role() {
    let owner = owner_pool().await;
    let a = seed_tenant(&owner, Uuid::now_v7()).await;
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT c.oid::regclass::text FROM pg_class c \
           JOIN pg_namespace ns ON ns.oid = c.relnamespace \
           JOIN pg_attribute a ON a.attrelid = c.oid AND a.attname = 'tenant_id' \
            AND a.attnum > 0 AND NOT a.attisdropped \
          WHERE c.relkind IN ('r', 'p') AND ns.nspname NOT IN ('pg_catalog', 'information_schema') \
            AND ns.nspname NOT LIKE 'pg\\_%' ORDER BY 1",
    )
    .fetch_all(&owner)
    .await
    .unwrap();
    assert!(tables.len() >= 300, "{} tenant tables", tables.len());

    let mut c = runtime_conn().await;
    let mut leaks = Vec::new();
    let mut unreadable = Vec::new();
    for (label, tenant, scope) in [
        ("unknown tenant", Some(Uuid::now_v7()), "tenant"),
        ("no context", None, ""),
    ] {
        set_ctx(&mut c, tenant, None, scope).await;
        for t in &tables {
            match sqlx::query_scalar::<_, i64>(&format!("SELECT count(*) FROM {t}"))
                .fetch_one(&mut c)
                .await
            {
                Ok(0) => {}
                Ok(n) => leaks.push(format!("{t} ({label}): {n}")),
                Err(e) if sqlstate(&e).as_deref() == Some("42501") => {
                    if label == "no context" {
                        unreadable.push(t.clone());
                    }
                }
                Err(e) => panic!("{t}: {e}"),
            }
        }
    }
    set_ctx(&mut c, Some(a.tenant), None, "tenant").await;
    for t in &tables {
        let sql = format!("SELECT count(*) FROM {t} WHERE tenant_id::text IS DISTINCT FROM $1");
        match sqlx::query_scalar::<_, i64>(&sql)
            .bind(a.tenant.to_string())
            .fetch_one(&mut c)
            .await
        {
            Ok(0) => {}
            Ok(n) => leaks.push(format!("{t} (tenant A context, foreign rows): {n}")),
            Err(e) if sqlstate(&e).as_deref() == Some("42501") => {}
            Err(e) => panic!("{t}: {e}"),
        }
    }
    assert!(
        leaks.is_empty(),
        "rows of another tenant visible: {leaks:?}"
    );
    // The sweep is meaningful: tenant A's own seeded rows are visible in its context.
    for (table, id) in a.targets() {
        assert_eq!(count_by_id(&mut c, table, id).await, 1, "{table}");
    }
    println!(
        "swept {} tenant tables; not readable by the runtime role: {unreadable:?}",
        tables.len()
    );
}

async fn student_visible(pool: &PgPool, id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM student.student WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn conn_settings(pool: &PgPool) -> (String, String, String) {
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query_as(
        "SELECT coalesce(current_setting('sutra.tenant_id', true), ''), \
                coalesce(current_setting('sutra.institution_id', true), ''), \
                coalesce(current_setting('sutra.rls_scope', true), '')",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap()
}

#[tokio::test]
async fn pooled_connections_never_carry_a_tenant_into_the_next_task() {
    let owner = owner_pool().await;
    let a = seed_tenant(&owner, Uuid::now_v7()).await;
    let scope_a = ExecutionScope::tenant(TenantId(a.tenant));
    let scope_b = ExecutionScope::tenant(TenantId(Uuid::now_v7()));

    // One physical connection, reused by every step below.
    let pool = runtime_pool(1);
    let pid = |p: PgPool| async move {
        sqlx::query_scalar::<_, i32>("SELECT pg_backend_pid()")
            .fetch_one(&p)
            .await
            .unwrap()
    };
    let first_pid = db_context::scoped(scope_a, pid(pool.clone())).await;
    db_context::scoped(scope_a, async {
        assert_eq!(student_visible(&pool, a.student).await, 1);
        let (t, i, k) = conn_settings(&pool).await;
        assert_eq!(
            (t, i, k.as_str()),
            (a.tenant.to_string(), String::new(), "tenant")
        );
        // A transaction inside the task keeps the context too.
        let mut tx = pool.begin().await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM student.student WHERE id = $1")
            .bind(a.student)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(n, 1);
        tx.rollback().await.unwrap();
    })
    .await;
    // Same connection, next task without a context: tenant A's settings are gone.
    assert_eq!(pid(pool.clone()).await, first_pid, "same pooled connection");
    assert_eq!(
        conn_settings(&pool).await,
        (String::new(), String::new(), String::new()),
        "a released connection must not keep the previous task's tenant"
    );
    assert_eq!(student_visible(&pool, a.student).await, 0);
    // Next task of tenant B on the same connection.
    db_context::scoped(scope_b, async {
        assert_eq!(student_visible(&pool, a.student).await, 0);
    })
    .await;
    // Server-internal work (relay, sweepers) is cross-tenant by design.
    db_context::system(async {
        assert_eq!(student_visible(&pool, a.student).await, 1);
    })
    .await;
    assert_eq!(student_visible(&pool, a.student).await, 0);

    // A task spawned inside a tenant-A request does not inherit the context: fails closed.
    let spawned = db_context::scoped(scope_a, async {
        let (p, id) = (pool.clone(), a.student);
        tokio::spawn(async move { student_visible(&p, id).await })
            .await
            .unwrap()
    })
    .await;
    assert_eq!(spawned, 0);

    // Concurrent tenant-A / tenant-B tasks sharing two connections.
    let pool2 = runtime_pool(2);
    for round in 0..25 {
        let (x, y) = tokio::join!(
            db_context::scoped(scope_a, student_visible(&pool2, a.student)),
            db_context::scoped(scope_b, student_visible(&pool2, a.student)),
        );
        assert_eq!((x, y), (1, 0), "round {round}");
    }
    assert_eq!(student_visible(&pool2, a.student).await, 0);
}

#[tokio::test]
async fn pinned_campus_session_cannot_reach_another_campus() {
    let owner = owner_pool().await;
    let a = seed_tenant(&owner, Uuid::now_v7()).await;
    let mut c = runtime_conn().await;

    // Tenant-wide session: both campuses.
    set_ctx(&mut c, Some(a.tenant), None, "tenant").await;
    assert_eq!(count_by_id(&mut c, "finance.charge", a.charge).await, 1);
    assert_eq!(
        count_by_id(&mut c, "finance.charge", a.other_campus_charge).await,
        1
    );

    // Pinned to institution 1: institution 2's rows are invisible and immutable.
    set_ctx(&mut c, Some(a.tenant), Some(a.institution), "tenant").await;
    assert_eq!(count_by_id(&mut c, "finance.charge", a.charge).await, 1);
    assert_eq!(
        count_by_id(&mut c, "finance.charge", a.other_campus_charge).await,
        0
    );
    assert_eq!(
        affected_or_refused(
            &mut c,
            "UPDATE finance.charge SET updated_at = now() WHERE id = $1",
            a.other_campus_charge
        )
        .await,
        0
    );
    // … and a row cannot be moved to institution 2.
    let moved = sqlx::query("UPDATE finance.charge SET institution_id = $2 WHERE id = $1")
        .bind(a.charge)
        .bind(a.other_institution)
        .execute(&mut c)
        .await
        .expect_err("moving a row to another campus must be refused");
    assert_eq!(sqlstate(&moved).as_deref(), Some("42501"), "{moved}");
    // The student persona (tenant-level) is visible where it has a context.
    assert_eq!(count_by_id(&mut c, "student.student", a.student).await, 1);

    // Pinned to institution 2: institution 1's student, document, result, staff, payroll run hidden.
    set_ctx(&mut c, Some(a.tenant), Some(a.other_institution), "tenant").await;
    for (table, id) in a.targets() {
        assert_eq!(
            count_by_id(&mut c, table, id).await,
            0,
            "{table}: a session pinned to another campus must not see it"
        );
    }
    assert_eq!(
        count_by_id(&mut c, "finance.charge", a.other_campus_charge).await,
        1
    );

    // The same through the API's pool and the session scope type.
    let pool = runtime_pool(1);
    let pinned =
        ExecutionScope::institution(TenantId(a.tenant), InstitutionId(a.other_institution));
    let n = db_context::scoped(pinned, async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM finance.charge WHERE id = ANY($1)")
            .bind(vec![a.charge, a.other_campus_charge])
            .fetch_one(&pool)
            .await
            .unwrap()
    })
    .await;
    assert_eq!(n, 1, "only the pinned campus's charge");
}
