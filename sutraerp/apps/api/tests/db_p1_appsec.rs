//! Part 1 · Stage 3 — application security (S3-appsec). Attack-style regression tests on the
//! real composition root and PostgreSQL (docs/readiness/security/appsec.md):
//!
//! * `append_only_evidence_cannot_be_truncated` (SEC-P1-APPSEC-001) — the general ledger, the
//!   finance status history and the audit trail refuse `TRUNCATE … CASCADE` even from the schema
//!   owner (row-level append-only triggers never fire on TRUNCATE);
//! * `posted_manual_journal_is_immutable_in_the_database` (SEC-P1-APPSEC-002) — once a manual
//!   journal is posted, direct SQL can no longer rewrite its amount / date / description, delete or
//!   rewrite its lines, delete it, move it back to draft or unlink its ledger entries; drafts stay
//!   editable through the API and the governed reversal still works;
//! * `search_terms_are_literal_not_sql_or_wildcards` (SEC-P1-APPSEC-003) — classic SQL-injection
//!   payloads and LIKE wildcards in the guardian search are matched literally (no 500, no
//!   "list everything", the table survives a `DROP TABLE` payload).
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use axum::Router;
use serde_json::{json, Value};
use support::*;

const T_TRUNCATE: &str = "0192f5a0-7c3e-7000-8000-0000000a5e01";
const T_JOURNAL: &str = "0192f5a0-7c3e-7000-8000-0000000a5e02";
const T_SEARCH: &str = "0192f5a0-7c3e-7000-8000-0000000a5e03";

const F: &str = "/api/v1/finance";
const S: &str = "/api/v1/student";

/// The TRUNCATE probe takes ACCESS EXCLUSIVE locks on many tables: keep this file's tests from
/// interleaving with it (deadlock detection would otherwise abort a probe with another error).
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn ver(v: &Value) -> i64 {
    v["version"].as_i64().unwrap()
}

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_owned()
}

async fn call(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
    expect: StatusCode,
) -> Value {
    let (s, _, b) = send(app, write(who, method, path, body, if_match)).await;
    assert_eq!(s, expect, "{method} {path}: {b}");
    b
}

async fn read(app: &Router, who: &Login, path: &str) -> Value {
    let (s, _, b) = send(app, get(who, path)).await;
    assert_eq!(s, StatusCode::OK, "GET {path}: {b}");
    b
}

/// Runs `sql` (one uuid bind) on the owner pool and returns the database error text (panics if
/// the statement succeeds).
async fn refused(pool: &sqlx::PgPool, sql: &str, bind: uuid::Uuid) -> String {
    match sqlx::query(sql).bind(bind).execute(pool).await {
        Ok(r) => panic!(
            "`{sql}` must be refused by the database, it changed {} row(s)",
            r.rows_affected()
        ),
        Err(e) => e.to_string(),
    }
}

/// Percent-encodes a query value (every byte outside the unreserved set).
fn qs(v: &str) -> String {
    v.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

// ------------------------------------------------------------------------------ SEC-P1-APPSEC-001

#[tokio::test]
async fn append_only_evidence_cannot_be_truncated() {
    let _serial = SERIAL.lock().await;
    let (_app, pool) = app(T_TRUNCATE).await;
    for table in [
        "platform.audit_log",
        "platform.audit_chain_entry",
        "platform.audit_anchor",
        "finance.journal_entry",
        "finance.journal_line",
        "finance.status_history",
        "finance.charge_line",
        "finance.trial_balance",
        "finance.gateway_webhook_event",
    ] {
        // Always rolled back: if the guard were missing the probe must not destroy shared data.
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("SET LOCAL lock_timeout = '60s'")
            .execute(&mut *tx)
            .await
            .unwrap();
        let res = sqlx::query(&format!("TRUNCATE {table} CASCADE"))
            .execute(&mut *tx)
            .await;
        tx.rollback().await.unwrap();
        let err = match res {
            Ok(_) => panic!("TRUNCATE {table} must be refused"),
            Err(e) => e.to_string(),
        };
        assert!(
            err.contains("cannot be truncated"),
            "TRUNCATE {table}: unexpected error {err}"
        );
    }
}

// ------------------------------------------------------------------------------ SEC-P1-APPSEC-002

#[tokio::test]
async fn posted_manual_journal_is_immutable_in_the_database() {
    let _serial = SERIAL.lock().await;
    let (app, pool) = app(T_JOURNAL).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup =
        b5::finance_setup(&app, &admin, &pool, T_JOURNAL, "Ledger Guard College", None).await;
    let inst = setup.institution().as_uuid();
    let chart = read(
        &app,
        &admin,
        &format!("{F}/institutions/{inst}/ledger-accounts"),
    )
    .await;
    let sys = |key: &str| {
        id(chart["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["systemKey"] == key)
            .unwrap())
    };
    let (bank, other) = (sys("bank_main"), sys("other_income"));
    let lines = |amount: &str| {
        json!([{"ledgerAccountId": bank, "debit": amount},
               {"ledgerAccountId": other, "credit": amount}])
    };

    // Drafts stay editable through the API (lines are replaced as a whole).
    let mj = call(
        &app,
        &admin,
        "POST",
        &format!("{F}/institutions/{inst}/manual-journals"),
        json!({"postingDate": "2026-07-10", "description": "Sundry income", "lines": lines("700.00")}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let mj = call(
        &app,
        &admin,
        "PUT",
        &format!("{F}/manual-journals/{}", id(&mj)),
        json!({"postingDate": "2026-07-10", "description": "Sundry income July", "lines": lines("750.00")}),
        Some(ver(&mj)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(mj["totalAmount"], "750.00");
    let posted = call(
        &app,
        &admin,
        "POST",
        &format!("{F}/manual-journals/{}/submit", id(&mj)),
        json!({}),
        Some(ver(&mj)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(posted["status"], "posted");
    let mid: uuid::Uuid = id(&mj).parse().unwrap();

    // Direct SQL tampering with the posted journal is refused by the database.
    let tamper = [
        (
            "UPDATE finance.manual_journal SET total_amount = 1.00 WHERE id = $1",
            "content is immutable",
        ),
        (
            "UPDATE finance.manual_journal SET posting_date = posting_date - 40 WHERE id = $1",
            "content is immutable",
        ),
        (
            "UPDATE finance.manual_journal SET description = 'tampered' WHERE id = $1",
            "content is immutable",
        ),
        (
            "UPDATE finance.manual_journal SET status = 'draft' WHERE id = $1",
            "can only be reversed",
        ),
        (
            "UPDATE finance.manual_journal SET journal_entry_id = NULL WHERE id = $1",
            "write-once",
        ),
        (
            "UPDATE finance.manual_journal SET decided_by = 'someone-else' WHERE id = $1",
            "decision record is immutable",
        ),
        (
            "DELETE FROM finance.manual_journal WHERE id = $1",
            "cannot be deleted",
        ),
        (
            "UPDATE finance.manual_journal_line SET debit = debit * 10 \
             WHERE manual_journal_id = $1 AND debit > 0",
            "lines are immutable",
        ),
        (
            "DELETE FROM finance.manual_journal_line WHERE manual_journal_id = $1",
            "lines are immutable",
        ),
        (
            "INSERT INTO finance.manual_journal_line (tenant_id, manual_journal_id, line_no, \
             ledger_account_id, debit) SELECT tenant_id, manual_journal_id, 99, ledger_account_id, 5 \
             FROM finance.manual_journal_line WHERE manual_journal_id = $1 LIMIT 1",
            "lines are immutable",
        ),
    ];
    for (sql, why) in tamper {
        let err = refused(&pool, sql, mid).await;
        assert!(err.contains(why), "`{sql}`: expected '{why}', got {err}");
    }
    let (amount, n): (rust_decimal::Decimal, i64) = sqlx::query_as(
        "SELECT j.total_amount, (SELECT count(*) FROM finance.manual_journal_line l \
         WHERE l.manual_journal_id = j.id) FROM finance.manual_journal j WHERE j.id = $1",
    )
    .bind(mid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(amount.to_string(), "750.00");
    assert_eq!(n, 2);

    // The governed reversal still works; a reversed journal is final.
    let entry = posted["journalEntryId"].as_str().unwrap().to_owned();
    call(
        &app,
        &admin,
        "POST",
        &format!("{F}/journal-entries/{entry}/reverse"),
        json!({"reason": "posted to the wrong account", "postingDate": "2026-07-12"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let reversed = read(&app, &admin, &format!("{F}/manual-journals/{mid}")).await;
    assert_eq!(reversed["status"], "reversed");
    assert!(reversed["reversalEntryId"].is_string());
    for (sql, why) in [
        (
            "UPDATE finance.manual_journal SET reversal_entry_id = NULL WHERE id = $1",
            "write-once",
        ),
        (
            "UPDATE finance.manual_journal SET status = 'posted' WHERE id = $1",
            "is final",
        ),
    ] {
        let err = refused(&pool, sql, mid).await;
        assert!(err.contains(why), "`{sql}`: expected '{why}', got {err}");
    }
}

// ------------------------------------------------------------------------------ SEC-P1-APPSEC-003

#[tokio::test]
async fn search_terms_are_literal_not_sql_or_wildcards() {
    let _serial = SERIAL.lock().await;
    let (app, _pool) = app(T_SEARCH).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let inst = register_institution(&app, &admin, "Search Safety School").await;
    let base = format!("{S}/institutions/{inst}/guardians");
    call(
        &app,
        &admin,
        "POST",
        &base,
        json!({"givenName": "Kiran", "familyName": "Rao", "phone": "+919800000101"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &base,
        json!({"givenName": "Meena", "familyName": "Iyer", "phone": "+919800000102",
               "email": "meena.iyer@example.org"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let count = |v: Value| v["items"].as_array().unwrap().len();

    assert_eq!(count(read(&app, &admin, &base).await), 2);
    assert_eq!(
        count(read(&app, &admin, &format!("{base}?q=kiran")).await),
        1
    );
    for payload in [
        "%",
        "_",
        "%%",
        "\\",
        "' OR '1'='1",
        "' OR 1=1 --",
        "x' UNION SELECT to_jsonb(g) FROM student.guardian g --",
        "'; DROP TABLE student.guardian; --",
    ] {
        let (s, _, body) = send(&app, get(&admin, &format!("{base}?q={}", qs(payload)))).await;
        assert_eq!(s, StatusCode::OK, "q={payload:?}: {body}");
        assert_eq!(
            count(body),
            0,
            "q={payload:?} must be matched literally, not as SQL or a wildcard"
        );
    }
    // The table survived the DROP payload.
    assert_eq!(count(read(&app, &admin, &base).await), 2);
}
