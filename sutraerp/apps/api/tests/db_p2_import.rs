//! Part 2 · Stage 4.4 (S4-import) — CSV / XLSX data migration against PostgreSQL, through the
//! real composition root (`/api/v1/platform/imports`, every module's import target).
//!
//! * `dry_run_reports_row_errors_and_duplicates` — column errors (required, date, e-mail), domain
//!   errors from the owning module (unknown programme, guardian without contact), in-file
//!   duplicates by natural key and rows already in the database (skip) are reported per row and
//!   in the error report; an invalid job cannot be committed (409) and nothing is written.
//! * `failed_batch_rolls_back_and_reimport_is_idempotent` — a batch that fails at commit (a
//!   conflict the per-batch dry run cannot see) is rolled back whole while the earlier batch stays;
//!   the corrected file resumes (committed rows skipped by natural key); re-uploading the same file
//!   answers with the same job (200) and a re-import under a new job key creates nothing.
//! * `opening_balances_need_a_second_approver_and_reconcile_to_the_ledger` — the commit waits for
//!   `platform.import.approve`; the maker (403), a user without the permission (403) and a
//!   rejection without a reason are refused; after approval by the finance checker the balances
//!   are posted through the charge flow (Dr student receivables / Cr accumulated fund) and the
//!   job's file total = committed total = ledger total; a re-import posts nothing more.
//! * `cross_tenant_jobs_and_institutions_are_invisible` — another tenant cannot read, dry-run,
//!   commit, decide, list or export a job / institution of this tenant (404) and cannot upload
//!   against its institution (404).
//! * `xlsx_round_trip_and_formula_injection_safe_export` — an XLSX upload imports; the CSV and
//!   XLSX exports neutralise cells a spreadsheet would evaluate (`=`, `+`, `-`, `@`); the exported
//!   workbook re-imports unchanged (every row skipped, nothing created).
//! * `two_thousand_students_import_timing` — 2,000 students with guardians from one CSV: upload,
//!   dry run and commit timings are printed (CI runs it with `--nocapture`).
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::{HeaderMap, StatusCode};
use http_body_util::BodyExt;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::str::FromStr;
use std::time::Instant;
use support::*;
use sutra_platform::dataimport::files;
use tower::ServiceExt;
use uuid::Uuid;

const T_DRY: &str = "0192f5a0-7c3e-7000-8000-0000000f4401";
const T_BATCH: &str = "0192f5a0-7c3e-7000-8000-0000000f4402";
const T_BAL: &str = "0192f5a0-7c3e-7000-8000-0000000f4403";
const T_ISO_A: &str = "0192f5a0-7c3e-7000-8000-0000000f4404";
const T_ISO_B: &str = "0192f5a0-7c3e-7000-8000-0000000f4405";
const T_XLSX: &str = "0192f5a0-7c3e-7000-8000-0000000f4406";
const T_PERF: &str = "0192f5a0-7c3e-7000-8000-0000000f4407";

const IMP: &str = "/api/v1/platform/imports";
const ADMIN: &str = "admin@sutra.dev";
const FINANCE_APPROVER: &str = "finance.approver@sutra.dev";
const FACULTY: &str = "meera.shah@sutra.dev";

const STUDENT_HEADER: &str = "student_number,given_name,family_name,date_of_birth,email,\
    programme_code,enrolment_number,guardian_name,guardian_relationship,guardian_phone";

// ------------------------------------------------------------------ helpers

/// Standard base64 (with padding) — the upload body carries the file this way.
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
    app: &axum::Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let (s, _, b) = send(app, write(who, method, path, body, None)).await;
    (s, b)
}

async fn read(app: &axum::Router, who: &Login, path: &str) -> (StatusCode, Value) {
    let (s, _, b) = send(app, get(who, path)).await;
    (s, b)
}

/// GET returning the raw body (files).
async fn raw(app: &axum::Router, who: &Login, path: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
    let res = app.clone().oneshot(get(who, path)).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes.to_vec())
}

/// Uploads a file; `extra` (an object) is merged into the request body.
async fn upload(
    app: &axum::Router,
    who: &Login,
    entity: &str,
    institution: Uuid,
    file_name: &str,
    bytes: &[u8],
    extra: Value,
) -> (StatusCode, Value) {
    let mut body = json!({
        "entity": entity,
        "institutionId": institution,
        "fileName": file_name,
        "contentBase64": b64(bytes),
    });
    if let (Value::Object(b), Value::Object(e)) = (&mut body, extra) {
        b.extend(e);
    }
    call(app, who, "POST", IMP, body).await
}

async fn dry_run(app: &axum::Router, who: &Login, job: &str) -> Value {
    let (s, j) = call(app, who, "POST", &format!("{IMP}/{job}/dry-run"), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    j
}

async fn commit(app: &axum::Router, who: &Login, job: &str) -> (StatusCode, Value) {
    call(app, who, "POST", &format!("{IMP}/{job}/commit"), json!({})).await
}

fn id(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {v}"))
        .to_owned()
}

fn n(v: &Value) -> i64 {
    v.as_i64().unwrap_or_else(|| panic!("not a number: {v}"))
}

fn dec(v: &Value) -> Decimal {
    match v {
        Value::String(s) => Decimal::from_str(s).unwrap(),
        Value::Number(x) => Decimal::from_str(&x.to_string()).unwrap(),
        other => panic!("not a decimal: {other}"),
    }
}

fn csv(header: &str, rows: &[String]) -> Vec<u8> {
    let mut s = String::from(header);
    s.push('\n');
    for r in rows {
        s.push_str(r);
        s.push('\n');
    }
    s.into_bytes()
}

/// One student row (`STUDENT_HEADER` order) with a guardian.
fn student(number: &str, given: &str, enrolment: &str) -> String {
    format!(
        "{number},{given},Migrated,2005-04-12,{}@example.org,BTECH-CSE,{enrolment},\
         Meena Migrated,mother,+91 98200 00001",
        number.to_lowercase().replace(['/', '.'], "-")
    )
}

/// Unique prefix of this run's student / enrolment numbers (letters, digits, `-`).
fn prefix(tag: &str) -> String {
    format!(
        "{tag}{}",
        Uuid::now_v7().simple().to_string()[20..].to_uppercase()
    )
}

async fn students_with_prefix(owner: &sqlx::PgPool, tenant: &str, prefix: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM student.student WHERE tenant_id = $1::uuid \
         AND student_number LIKE $2",
    )
    .bind(tenant)
    .bind(format!("{prefix}%"))
    .fetch_one(owner)
    .await
    .unwrap()
}

/// Uploads, dry-runs and commits a students CSV; returns the committed job.
async fn import_students(app: &axum::Router, who: &Login, inst: Uuid, rows: &[String]) -> Value {
    let (s, job) = upload(
        app,
        who,
        "students",
        inst,
        "students.csv",
        &csv(STUDENT_HEADER, rows),
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{job}");
    let j = dry_run(app, who, &id(&job)).await;
    assert_eq!(j["status"], "validated", "{j}");
    let (s, j) = commit(app, who, &id(&job)).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["status"], "committed", "{j}");
    j
}

async fn audit_actions(owner: &sqlx::PgPool, tenant: &str, job: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT action FROM platform.audit_log WHERE tenant_id = $1::uuid AND target_id = $2 \
         AND outcome = 'succeeded' ORDER BY occurred_at",
    )
    .bind(tenant)
    .bind(job)
    .fetch_all(owner)
    .await
    .unwrap()
}

/// Opening-balance charges of the institution.
async fn opening_charges(owner: &sqlx::PgPool, inst: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM finance.charge WHERE institution_id = $1 \
         AND source_type = 'opening_balance'",
    )
    .bind(inst)
    .fetch_one(owner)
    .await
    .unwrap()
}

/// Balance (credit − debit) of the institution's ledger account with `system_key`.
async fn balance(owner: &sqlx::PgPool, inst: Uuid, key: &str) -> Decimal {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(l.credit - l.debit), 0) FROM finance.journal_line l \
         JOIN finance.ledger_account a ON a.id = l.ledger_account_id \
         WHERE a.institution_id = $1 AND a.system_key = $2",
    )
    .bind(inst)
    .bind(key)
    .fetch_one(owner)
    .await
    .unwrap()
}

// ------------------------------------------------------------------ dry run

#[tokio::test]
async fn dry_run_reports_row_errors_and_duplicates() {
    let (app, owner) = app(T_DRY).await;
    let admin = login(&app, ADMIN).await;
    let setup = b3::academic_setup(&app, &admin, T_DRY, "Import dry run").await;
    let inst = setup.institution_id;
    let p = prefix("D");

    // Two students already migrated (the database duplicates of the next file).
    import_students(
        &app,
        &admin,
        inst,
        &[
            student(&format!("{p}/0001"), "First", &format!("{p}-E1")),
            student(&format!("{p}/0002"), "Second", &format!("{p}-E2")),
        ],
    )
    .await;
    assert_eq!(students_with_prefix(&owner, T_DRY, &p).await, 2);

    let rows = vec![
        // line 2: already in the database → skip
        student(&format!("{p}/0001"), "First", &format!("{p}-E1")),
        // line 3: new → create
        student(&format!("{p}/0003"), "Third", &format!("{p}-E3")),
        // line 4: same student number as line 3 → duplicate in file
        student(&format!("{p}/0003"), "Third again", &format!("{p}-E4")),
        // line 5: given name missing → required
        student(&format!("{p}/0005"), "", &format!("{p}-E5")),
        // line 6: date of birth in the wrong form → date
        format!(
            "{p}/0006,Sixth,Migrated,31-31-2005,six@example.org,BTECH-CSE,{p}-E6,,,"
        ),
        // line 7: unknown programme → the student module's validation
        format!("{p}/0007,Seventh,Migrated,2005-04-12,seven@example.org,NOPE-PROG,{p}-E7,,,"),
        // line 8: not an e-mail address → email
        format!("{p}/0008,Eighth,Migrated,2005-04-12,not-an-email,BTECH-CSE,{p}-E8,,,"),
        // line 9: a guardian without phone or e-mail → the student module's validation
        format!(
            "{p}/0009,Ninth,Migrated,2005-04-12,nine@example.org,BTECH-CSE,{p}-E9,Ravi Kumar,father,"
        ),
    ];
    let (s, job) = upload(
        &app,
        &admin,
        "students",
        inst,
        "legacy-students.csv",
        &csv(STUDENT_HEADER, &rows),
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{job}");
    assert_eq!(job["status"], "uploaded");
    assert_eq!(job["fileFormat"], "csv");
    assert_eq!(n(&job["summary"]["rowsInFile"]), 8);
    // Every header was matched to its column.
    assert_eq!(job["mapping"]["programme_code"], "programme_code", "{job}");
    let jid = id(&job);

    // Commit before a clean dry run → 409.
    let (s, _) = commit(&app, &admin, &jid).await;
    assert_eq!(s, StatusCode::CONFLICT);

    let j = dry_run(&app, &admin, &jid).await;
    assert_eq!(j["status"], "invalid", "{j}");
    assert_eq!(n(&j["summary"]["errors"]), 6, "{j}");
    assert_eq!(n(&j["summary"]["duplicatesInFile"]), 1, "{j}");
    assert_eq!(n(&j["summary"]["existingInDatabase"]), 1, "{j}");
    assert_eq!(n(&j["actions"]["skip"]), 1, "{j}");
    assert_eq!(n(&j["actions"]["create"]), 1, "{j}");
    assert_eq!(n(&j["actions"]["error"]), 6, "{j}");
    assert!(j["validatedMs"].as_i64().is_some(), "{j}");

    let (s, errs) = read(&app, &admin, &format!("{IMP}/{jid}/rows?errorsOnly=true")).await;
    assert_eq!(s, StatusCode::OK, "{errs}");
    let items = errs["items"].as_array().unwrap();
    let by_line = |line: i64| -> Vec<(String, String)> {
        items
            .iter()
            .find(|r| r["line"].as_i64() == Some(line))
            .unwrap_or_else(|| panic!("line {line} not reported: {errs}"))["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["field"].as_str().unwrap().to_owned(),
                    e["code"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    assert_eq!(
        items.iter().map(|r| n(&r["line"])).collect::<Vec<_>>(),
        vec![4, 5, 6, 7, 8, 9],
        "{errs}"
    );
    assert!(by_line(4).iter().any(|(_, c)| c == "duplicate_in_file"));
    assert!(by_line(5).contains(&("given_name".into(), "required".into())));
    assert!(by_line(6).contains(&("date_of_birth".into(), "date".into())));
    assert!(by_line(7).contains(&("programme_code".into(), "unknown".into())));
    assert!(by_line(8).contains(&("email".into(), "email".into())));
    assert!(by_line(9).contains(&("guardian_phone".into(), "required".into())));
    for r in items {
        assert_eq!(r["action"], "error");
        assert_eq!(r["committed"], false);
    }

    // All rows: line 2 skip (existing), line 3 create.
    let (_, all) = read(&app, &admin, &format!("{IMP}/{jid}/rows")).await;
    let all = all["items"].as_array().unwrap();
    assert_eq!(all.len(), 8);
    assert_eq!(all[0]["action"], "skip");
    assert!(all[0]["recordId"].as_str().is_some());
    assert_eq!(all[1]["action"], "create");

    // Error report: one line per violation with the source cells.
    let (s, h, report) = raw(&app, &admin, &format!("{IMP}/{jid}/errors.csv")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(h["content-type"].to_str().unwrap().starts_with("text/csv"));
    let report = String::from_utf8(report).unwrap();
    let report = report.trim_start_matches('\u{feff}');
    assert!(
        report.starts_with("line,column,code,message,student_number"),
        "{report}"
    );
    for needle in [
        "duplicate_in_file",
        "date_of_birth,date",
        "programme_code,unknown",
        "NOPE-PROG",
    ] {
        assert!(report.contains(needle), "{needle} missing in {report}");
    }

    // Still invalid: nothing can be committed, nothing was written by the dry run.
    let (s, _) = commit(&app, &admin, &jid).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(students_with_prefix(&owner, T_DRY, &p).await, 2);

    // Without the student module's permission the job is not reachable for writes (faculty).
    let faculty = login(&app, FACULTY).await;
    let (s, _) = call(
        &app,
        &faculty,
        "POST",
        &format!("{IMP}/{jid}/dry-run"),
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let audit = audit_actions(&owner, T_DRY, &jid).await;
    assert!(
        audit.contains(&"platform.import.upload".to_owned()),
        "{audit:?}"
    );
    assert!(
        audit.contains(&"platform.import.dry_run".to_owned()),
        "{audit:?}"
    );
}

// ------------------------------------------------------------------ batches, idempotency

#[tokio::test]
async fn failed_batch_rolls_back_and_reimport_is_idempotent() {
    let (app, owner) = app(T_BATCH).await;
    let admin = login(&app, ADMIN).await;
    let setup = b3::academic_setup(&app, &admin, T_BATCH, "Import batches").await;
    let inst = setup.institution_id;
    let p = prefix("B");
    let s1 = format!("{p}/1");
    let s2 = format!("{p}/2");
    let s3 = format!("{p}/3");
    let s4 = format!("{p}/4");
    let (e1, e2, e3, e4) = (
        format!("{p}-E1"),
        format!("{p}-E2"),
        format!("{p}-E3"),
        format!("{p}-E4"),
    );

    // Two rows per batch; line 5 reuses line 2's enrolment number. Each dry-run batch is rolled
    // back on its own, so the conflict only shows when batch 1 is really committed.
    let bad = csv(
        STUDENT_HEADER,
        &[
            student(&s1, "One", &e1),
            student(&s2, "Two", &e2),
            student(&s3, "Three", &e3),
            student(&s4, "Four", &e1),
        ],
    );
    let (s, job) = upload(
        &app,
        &admin,
        "students",
        inst,
        "batch.csv",
        &bad,
        json!({"batchSize": 2}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{job}");
    let bad_id = id(&job);
    let j = dry_run(&app, &admin, &bad_id).await;
    assert_eq!(j["status"], "validated", "{j}");

    let (s, j) = commit(&app, &admin, &bad_id).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["status"], "failed", "{j}");
    let reason = j["failureReason"].as_str().unwrap();
    assert!(reason.contains("batch 2 was rolled back"), "{reason}");
    assert!(reason.contains("line 5"), "{reason}");
    assert_eq!(n(&j["summary"]["batchesCommitted"]), 1, "{j}");
    assert_eq!(n(&j["summary"]["created"]), 2, "{j}");
    // Batch 1 stays; the whole of batch 2 (line 4 was fine on its own) is rolled back.
    assert_eq!(students_with_prefix(&owner, T_BATCH, &p).await, 2);
    let s3_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM student.student WHERE tenant_id = $1::uuid AND student_number = $2",
    )
    .bind(T_BATCH)
    .bind(&s3)
    .fetch_one(&owner)
    .await
    .unwrap();
    assert_eq!(s3_rows, 0);
    let (_, rows) = read(&app, &admin, &format!("{IMP}/{bad_id}/rows")).await;
    let rows = rows["items"].as_array().unwrap().clone();
    assert_eq!(rows[0]["committed"], true);
    assert_eq!(rows[1]["committed"], true);
    assert_eq!(rows[2]["committed"], false);
    assert_eq!(rows[3]["committed"], false);
    assert_eq!(rows[3]["action"], "error", "{:?}", rows[3]);
    assert_eq!(rows[3]["errors"][0]["field"], "enrolment_number");
    assert_eq!(rows[3]["errors"][0]["code"], "duplicate");

    // Re-validating the failed job now sees the committed rows (skip) and the real conflict.
    let j = dry_run(&app, &admin, &bad_id).await;
    assert_eq!(j["status"], "invalid", "{j}");
    assert_eq!(n(&j["actions"]["skip"]), 2, "{j}");
    assert_eq!(n(&j["actions"]["error"]), 1, "{j}");

    // The corrected file resumes: rows already committed are skipped by natural key.
    let fixed = csv(
        STUDENT_HEADER,
        &[
            student(&s1, "One", &e1),
            student(&s2, "Two", &e2),
            student(&s3, "Three", &e3),
            student(&s4, "Four", &e4),
        ],
    );
    let (s, job) = upload(
        &app,
        &admin,
        "students",
        inst,
        "batch-fixed.csv",
        &fixed,
        json!({"batchSize": 2}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{job}");
    let fixed_id = id(&job);
    let j = dry_run(&app, &admin, &fixed_id).await;
    assert_eq!(j["status"], "validated", "{j}");
    assert_eq!(n(&j["summary"]["existingInDatabase"]), 2, "{j}");
    let (s, j) = commit(&app, &admin, &fixed_id).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["status"], "committed", "{j}");
    assert_eq!(n(&j["summary"]["created"]), 2, "{j}");
    assert_eq!(n(&j["summary"]["skipped"]), 2, "{j}");
    assert_eq!(n(&j["summary"]["batchesCommitted"]), 2, "{j}");
    assert!(j["committedMs"].as_i64().is_some());
    assert_eq!(j["committedBy"], "dev|admin@sutra.dev");
    assert_eq!(students_with_prefix(&owner, T_BATCH, &p).await, 4);
    let guardians: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM student.guardian_relationship r JOIN student.student s \
         ON s.id = r.student_id WHERE s.tenant_id = $1::uuid AND s.student_number LIKE $2",
    )
    .bind(T_BATCH)
    .bind(format!("{p}%"))
    .fetch_one(&owner)
    .await
    .unwrap();
    assert_eq!(
        guardians, 4,
        "one proposed guardian relationship per student"
    );

    // Same file again (default job key = entity + institution + SHA-256) → the same job, 200.
    let (s, again) = upload(
        &app,
        &admin,
        "students",
        inst,
        "batch-fixed.csv",
        &fixed,
        json!({"batchSize": 2}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{again}");
    assert_eq!(id(&again), fixed_id);
    assert_eq!(again["status"], "committed");
    // The same job key with another file → 409.
    let key = format!("legacy-{p}");
    let (s, k1) = upload(
        &app,
        &admin,
        "students",
        inst,
        "rerun.csv",
        &fixed,
        json!({"jobKey": key}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{k1}");
    let (s, _) = upload(
        &app,
        &admin,
        "students",
        inst,
        "rerun.csv",
        &bad,
        json!({"jobKey": key}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // A re-import under a new job key creates nothing.
    let rerun = id(&k1);
    let j = dry_run(&app, &admin, &rerun).await;
    assert_eq!(j["status"], "validated", "{j}");
    assert_eq!(n(&j["actions"]["skip"]), 4, "{j}");
    let (s, j) = commit(&app, &admin, &rerun).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["status"], "committed", "{j}");
    assert_eq!(n(&j["summary"]["created"]), 0, "{j}");
    assert_eq!(n(&j["summary"]["skipped"]), 4, "{j}");
    assert_eq!(students_with_prefix(&owner, T_BATCH, &p).await, 4);
    // A committed job cannot be committed or dry-run again.
    let (s, _) = commit(&app, &admin, &rerun).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{IMP}/{rerun}/dry-run"),
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    let audit = audit_actions(&owner, T_BATCH, &bad_id).await;
    assert!(
        audit.contains(&"platform.import.commit".to_owned()),
        "{audit:?}"
    );
}

// ------------------------------------------------------------------ opening balances

#[tokio::test]
async fn opening_balances_need_a_second_approver_and_reconcile_to_the_ledger() {
    let (app, owner) = app(T_BAL).await;
    let admin = login(&app, ADMIN).await;
    let approver = login(&app, FINANCE_APPROVER).await;
    let faculty = login(&app, FACULTY).await;
    let setup = b3::academic_setup(&app, &admin, T_BAL, "Import balances").await;
    let inst = setup.institution_id;
    sqlx::query("UPDATE institution.institution SET status = 'active' WHERE id = $1")
        .bind(inst)
        .execute(&owner)
        .await
        .unwrap();
    let p = prefix("OB");
    let numbers: Vec<String> = (1..=3).map(|i| format!("{p}/{i}")).collect();
    import_students(
        &app,
        &admin,
        inst,
        &numbers
            .iter()
            .enumerate()
            .map(|(i, s)| student(s, "Payer", &format!("{p}-E{i}")))
            .collect::<Vec<_>>(),
    )
    .await;

    let header = "student_number,amount,due_date,description";
    let file = csv(
        header,
        &[
            format!("{},\"12,500.00\",2026-12-31,Fees carried over", numbers[0]),
            format!("{},4999.50,,", numbers[1]),
            format!("{},100.25,2026-12-31,Library dues", numbers[2]),
        ],
    );
    let total = Decimal::from_str("17599.75").unwrap();

    // The checker cannot raise the import (no platform.import.manage).
    let (s, _) = upload(
        &app,
        &approver,
        "opening_balances",
        inst,
        "balances.csv",
        &file,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let (s, job) = upload(
        &app,
        &admin,
        "opening_balances",
        inst,
        "balances.csv",
        &file,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{job}");
    assert_eq!(job["requiresApproval"], true);
    let jid = id(&job);
    let j = dry_run(&app, &admin, &jid).await;
    assert_eq!(j["status"], "validated", "{j}");
    assert_eq!(dec(&j["summary"]["fileTotal"]), total, "{j}");

    // Nothing to decide yet → 409.
    let (s, _) = call(
        &app,
        &approver,
        "POST",
        &format!("{IMP}/{jid}/decision"),
        json!({"approve": true}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // The commit request waits for a second person; nothing is posted yet.
    let (s, j) = commit(&app, &admin, &jid).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["status"], "pending_approval", "{j}");
    assert_eq!(j["commitRequestedBy"], "dev|admin@sutra.dev");
    assert!(j["processInstanceId"].as_str().is_some(), "{j}");
    assert_eq!(opening_charges(&owner, inst).await, 0);
    // A second commit request while pending → 409.
    let (s, _) = commit(&app, &admin, &jid).await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Separation of duties: the maker cannot decide (403); faculty lacks the permission (403).
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{IMP}/{jid}/decision"),
        json!({"approve": true, "reason": "my own import"}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
    let (s, _) = call(
        &app,
        &faculty,
        "POST",
        &format!("{IMP}/{jid}/decision"),
        json!({"approve": true}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(opening_charges(&owner, inst).await, 0);

    // A rejection needs a reason; with one, the job is rejected and nothing is posted.
    let (s, b) = call(
        &app,
        &approver,
        "POST",
        &format!("{IMP}/{jid}/decision"),
        json!({"approve": false}),
    )
    .await;
    assert!(
        s.is_client_error() && s != StatusCode::FORBIDDEN && s != StatusCode::NOT_FOUND,
        "{s} {b}"
    );
    let (s, j) = call(
        &app,
        &approver,
        "POST",
        &format!("{IMP}/{jid}/decision"),
        json!({"approve": false, "reason": "due dates to be confirmed with accounts"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["status"], "rejected", "{j}");
    assert_eq!(j["decidedBy"], "dev|finance.approver@sutra.dev");
    assert_eq!(opening_charges(&owner, inst).await, 0);

    // Dry-run again, request again, the finance checker approves: posted and reconciled.
    let j = dry_run(&app, &admin, &jid).await;
    assert_eq!(j["status"], "validated", "{j}");
    let (s, j) = commit(&app, &admin, &jid).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["status"], "pending_approval", "{j}");
    let (s, j) = call(
        &app,
        &approver,
        "POST",
        &format!("{IMP}/{jid}/decision"),
        json!({"approve": true, "reason": "agrees with the legacy ledger"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["status"], "committed", "{j}");
    assert_eq!(j["decidedBy"], "dev|finance.approver@sutra.dev");
    assert_eq!(j["decisionReason"], "agrees with the legacy ledger");
    assert_eq!(n(&j["summary"]["created"]), 3, "{j}");
    assert_eq!(dec(&j["summary"]["fileTotal"]), total);
    assert_eq!(dec(&j["summary"]["committedTotal"]), total);
    assert_eq!(dec(&j["summary"]["ledgerTotal"]), total);
    assert_eq!(j["summary"]["reconciled"], true, "{j}");

    // Independently from the ledger: Dr student receivables / Cr accumulated fund, balanced.
    assert_eq!(opening_charges(&owner, inst).await, 3);
    let published: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM finance.charge WHERE institution_id = $1 \
         AND source_type = 'opening_balance' AND status = 'published'",
    )
    .bind(inst)
    .fetch_one(&owner)
    .await
    .unwrap();
    assert_eq!(published, 3);
    assert_eq!(balance(&owner, inst, "student_receivables").await, -total);
    assert_eq!(balance(&owner, inst, "accumulated_fund").await, total);
    let (dr, cr): (Decimal, Decimal) = sqlx::query_as(
        "SELECT COALESCE(sum(debit), 0), COALESCE(sum(credit), 0) FROM finance.journal_line \
         WHERE journal_entry_id IN (SELECT DISTINCT l.journal_entry_id FROM finance.journal_line l \
         JOIN finance.charge c ON c.id = l.charge_id \
         WHERE c.institution_id = $1 AND c.source_type = 'opening_balance')",
    )
    .bind(inst)
    .fetch_one(&owner)
    .await
    .unwrap();
    assert_eq!((dr, cr), (total, total));

    let audit = audit_actions(&owner, T_BAL, &jid).await;
    for a in [
        "platform.import.commit_request",
        "platform.import.reject",
        "platform.import.approve",
        "platform.import.commit",
    ] {
        assert!(audit.contains(&a.to_owned()), "{a} missing: {audit:?}");
    }

    // Re-importing the same balances (new job key) posts nothing more.
    let (s, job) = upload(
        &app,
        &admin,
        "opening_balances",
        inst,
        "balances.csv",
        &file,
        json!({"jobKey": format!("balances-again-{p}")}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{job}");
    let again = id(&job);
    let j = dry_run(&app, &admin, &again).await;
    assert_eq!(n(&j["actions"]["skip"]), 3, "{j}");
    let (_, j) = commit(&app, &admin, &again).await;
    assert_eq!(j["status"], "pending_approval", "{j}");
    let (s, j) = call(
        &app,
        &approver,
        "POST",
        &format!("{IMP}/{again}/decision"),
        json!({"approve": true}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["status"], "committed", "{j}");
    assert_eq!(n(&j["summary"]["created"]), 0, "{j}");
    assert_eq!(j["summary"]["reconciled"], true, "{j}");
    assert_eq!(opening_charges(&owner, inst).await, 3);
    assert_eq!(balance(&owner, inst, "student_receivables").await, -total);
}

// ------------------------------------------------------------------ tenant isolation

#[tokio::test]
async fn cross_tenant_jobs_and_institutions_are_invisible() {
    let (app_a, owner) = app(T_ISO_A).await;
    let (app_b, _) = app(T_ISO_B).await;
    let admin_a = login(&app_a, ADMIN).await;
    let admin_b = login(&app_b, ADMIN).await;
    let setup = b3::academic_setup(&app_a, &admin_a, T_ISO_A, "Import tenant A").await;
    let inst_a = setup.institution_id;
    let p = prefix("X");
    let file = csv(
        STUDENT_HEADER,
        &[student(&format!("{p}/1"), "Tenant", &format!("{p}-E1"))],
    );
    let (s, job) = upload(
        &app_a,
        &admin_a,
        "students",
        inst_a,
        "a.csv",
        &file,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{job}");
    let jid = id(&job);
    dry_run(&app_a, &admin_a, &jid).await;

    // Tenant B: every read and command on A's job is a 404.
    for path in [
        format!("{IMP}/{jid}"),
        format!("{IMP}/{jid}/rows"),
        format!("{IMP}/{jid}/errors.csv"),
    ] {
        let (s, _, _) = raw(&app_b, &admin_b, &path).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{path}");
    }
    for (method, path, body) in [
        ("POST", format!("{IMP}/{jid}/dry-run"), json!({})),
        ("POST", format!("{IMP}/{jid}/commit"), json!({})),
        (
            "POST",
            format!("{IMP}/{jid}/decision"),
            json!({"approve": false, "reason": "not ours"}),
        ),
        (
            "PUT",
            format!("{IMP}/{jid}/mapping"),
            json!({"mapping": {"student_number": "student_number"}}),
        ),
    ] {
        let (s, b) = call(&app_b, &admin_b, method, &path, body).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{method} {path}: {b}");
    }
    let (s, list) = read(&app_b, &admin_b, IMP).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        !list["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|j| j["id"] == jid.as_str()),
        "{list}"
    );
    // Nor can B record a job against, or export from, A's institution.
    let (s, b) = upload(
        &app_b,
        &admin_b,
        "students",
        inst_a,
        "b.csv",
        &file,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{b}");
    let (s, _, _) = raw(
        &app_b,
        &admin_b,
        &format!("/api/v1/platform/imports/entities/students/export?institutionId={inst_a}"),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let foreign: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.import_job WHERE tenant_id = $1::uuid AND institution_id = $2",
    )
    .bind(T_ISO_B)
    .bind(inst_a)
    .fetch_one(&owner)
    .await
    .unwrap();
    assert_eq!(foreign, 0);

    // A's job is untouched and still A's to commit.
    let (s, j) = commit(&app_a, &admin_a, &jid).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["status"], "committed", "{j}");
    assert_eq!(students_with_prefix(&owner, T_ISO_A, &p).await, 1);
    assert_eq!(students_with_prefix(&owner, T_ISO_B, &p).await, 0);
}

// ------------------------------------------------------------------ XLSX, export

#[tokio::test]
async fn xlsx_round_trip_and_formula_injection_safe_export() {
    let (app, owner) = app(T_XLSX).await;
    let admin = login(&app, ADMIN).await;
    let setup = b3::academic_setup(&app, &admin, T_XLSX, "Import xlsx").await;
    let inst = setup.institution_id;
    let p = prefix("XL");

    // Template downloads (both formats).
    let (s, h, t) = raw(
        &app,
        &admin,
        "/api/v1/platform/imports/entities/students/template?format=xlsx",
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(h["content-type"]
        .to_str()
        .unwrap()
        .contains("spreadsheetml"));
    let template = files::read_xlsx(&t).unwrap();
    assert_eq!(template.headers[0], "student_number");
    assert_eq!(template.rows.len(), 1, "one example row");

    // An XLSX upload whose cells would be formulas in a spreadsheet.
    let headers: Vec<String> = [
        "student_number",
        "given_name",
        "family_name",
        "date_of_birth",
        "phone",
        "programme_code",
        "enrolment_number",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let evil_given = "=HYPERLINK(\"http://evil.example\",\"x\")";
    let evil_family = "@SUM(A1:A2)";
    let rows = vec![
        vec![
            format!("{p}/1"),
            evil_given.to_owned(),
            evil_family.to_owned(),
            "2005-04-12".to_owned(),
            "+91 98200 00001".to_owned(),
            "BTECH-CSE".to_owned(),
            format!("{p}-E1"),
        ],
        vec![
            format!("{p}/2"),
            "Plain".to_owned(),
            "-Dash".to_owned(),
            "2006-01-30".to_owned(),
            String::new(),
            "BTECH-CSE".to_owned(),
            format!("{p}-E2"),
        ],
    ];
    let book = files::write_xlsx("students", &headers, &rows).unwrap();
    let (s, job) = upload(
        &app,
        &admin,
        "students",
        inst,
        "legacy.xlsx",
        &book,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{job}");
    assert_eq!(job["fileFormat"], "xlsx");
    let jid = id(&job);
    let j = dry_run(&app, &admin, &jid).await;
    assert_eq!(j["status"], "validated", "{j}");
    let (_, j) = commit(&app, &admin, &jid).await;
    assert_eq!(j["status"], "committed", "{j}");
    assert_eq!(n(&j["summary"]["created"]), 2, "{j}");
    // Stored as text, exactly as the legacy system had it.
    let (given, family, phone): (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT given_name, family_name, primary_phone FROM student.student \
         WHERE tenant_id = $1::uuid AND student_number = $2",
    )
    .bind(T_XLSX)
    .bind(format!("{p}/1"))
    .fetch_one(&owner)
    .await
    .unwrap();
    assert_eq!(given, evil_given);
    assert_eq!(family.as_deref(), Some(evil_family));
    assert_eq!(phone.as_deref(), Some("+91 98200 00001"));

    // CSV export: every cell a spreadsheet would evaluate carries the `'` guard.
    let export = format!("/api/v1/platform/imports/entities/students/export?institutionId={inst}");
    let (s, h, body) = raw(&app, &admin, &format!("{export}&format=csv")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(h["content-disposition"]
        .to_str()
        .unwrap()
        .contains("students-export.csv"));
    let text = String::from_utf8(body.clone()).unwrap();
    let text = text.trim_start_matches('\u{feff}');
    assert!(
        text.contains("\"'=HYPERLINK(\"\"http://evil.example\"\",\"\"x\"\")\""),
        "{text}"
    );
    assert!(text.contains(",'@SUM(A1:A2),"), "{text}");
    assert!(text.contains(",'-Dash,"), "{text}");
    assert!(text.contains("'+91 98200 00001"), "{text}");
    let parsed = files::read_csv(&body).unwrap();
    for (line, cells) in &parsed.rows {
        for c in cells {
            // read_csv strips the guard again; the raw text must not hold an unguarded formula.
            if c.starts_with(['=', '+', '-', '@']) {
                assert!(
                    text.contains(&format!("'{}", c.replace('"', "\"\""))),
                    "line {line}: {c} exported without the guard"
                );
            }
        }
    }
    let audit: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.audit_log WHERE tenant_id = $1::uuid \
         AND action = 'platform.import.export' AND target_id = 'students'",
    )
    .bind(T_XLSX)
    .fetch_one(&owner)
    .await
    .unwrap();
    assert!(audit >= 1);

    // XLSX export → re-import: the same data, every row skipped, nothing created.
    let (s, h, xlsx) = raw(&app, &admin, &format!("{export}&format=xlsx")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(h["content-type"]
        .to_str()
        .unwrap()
        .contains("spreadsheetml"));
    assert!(xlsx.starts_with(b"PK"), "an Office Open XML package");
    let table = files::read_xlsx(&xlsx).unwrap();
    let col = |name: &str| table.headers.iter().position(|h| h == name).unwrap();
    let mine: Vec<&Vec<String>> = table
        .rows
        .iter()
        .map(|(_, r)| r)
        .filter(|r| r[col("student_number")].starts_with(&p))
        .collect();
    assert_eq!(mine.len(), 2);
    let first = mine
        .iter()
        .find(|r| r[col("student_number")] == format!("{p}/1"))
        .unwrap();
    assert_eq!(first[col("given_name")], evil_given);
    assert_eq!(first[col("family_name")], evil_family);
    assert_eq!(first[col("programme_code")], "BTECH-CSE");
    assert_eq!(first[col("date_of_birth")], "2005-04-12");

    let (s, job) = upload(
        &app,
        &admin,
        "students",
        inst,
        "export.xlsx",
        &xlsx,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{job}");
    let rid = id(&job);
    let j = dry_run(&app, &admin, &rid).await;
    assert_eq!(j["status"], "validated", "{j}");
    assert_eq!(n(&j["actions"]["error"]), 0, "{j}");
    assert_eq!(n(&j["actions"]["create"]), 0, "{j}");
    let (_, j) = commit(&app, &admin, &rid).await;
    assert_eq!(j["status"], "committed", "{j}");
    assert_eq!(n(&j["summary"]["created"]), 0, "{j}");
    assert_eq!(students_with_prefix(&owner, T_XLSX, &p).await, 2);
}

// ------------------------------------------------------------------ volume

#[tokio::test]
async fn two_thousand_students_import_timing() {
    const N: usize = 2_000;
    let (app, owner) = app(T_PERF).await;
    let admin = login(&app, ADMIN).await;
    let setup = b3::academic_setup(&app, &admin, T_PERF, "Import volume").await;
    let inst = setup.institution_id;
    let p = prefix("V");
    let rows: Vec<String> = (1..=N)
        .map(|i| {
            student(
                &format!("{p}/{i:05}"),
                &format!("Student{i}"),
                &format!("{p}-{i:05}"),
            )
        })
        .collect();
    let file = csv(STUDENT_HEADER, &rows);

    let t0 = Instant::now();
    let (s, job) = upload(
        &app,
        &admin,
        "students",
        inst,
        "volume.csv",
        &file,
        json!({}),
    )
    .await;
    let upload_ms = t0.elapsed().as_millis();
    assert_eq!(s, StatusCode::CREATED, "{job}");
    assert_eq!(n(&job["summary"]["rowsInFile"]), N as i64);
    let jid = id(&job);

    let t1 = Instant::now();
    let j = dry_run(&app, &admin, &jid).await;
    let dry_ms = t1.elapsed().as_millis();
    assert_eq!(j["status"], "validated", "{}", j["summary"]);
    assert_eq!(n(&j["actions"]["create"]), N as i64);
    let dry_server = n(&j["validatedMs"]);

    let t2 = Instant::now();
    let (s, j) = commit(&app, &admin, &jid).await;
    let commit_ms = t2.elapsed().as_millis();
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["status"], "committed", "{j}");
    assert_eq!(n(&j["summary"]["created"]), N as i64);
    assert_eq!(
        n(&j["summary"]["batchesCommitted"]),
        4,
        "500 rows per batch"
    );
    let commit_server = n(&j["committedMs"]);
    let total_ms = t0.elapsed().as_millis();

    assert_eq!(students_with_prefix(&owner, T_PERF, &p).await, N as i64);
    let contexts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM student.student_context c JOIN student.student s \
         ON s.id = c.student_id WHERE c.institution_id = $1 AND s.student_number LIKE $2 \
         AND c.status = 'active'",
    )
    .bind(inst)
    .bind(format!("{p}%"))
    .fetch_one(&owner)
    .await
    .unwrap();
    assert_eq!(contexts, N as i64);

    println!(
        "S4-import TIMING: {N} students (+ {N} guardians) from one CSV ({} bytes): \
         upload {upload_ms} ms, dry run {dry_ms} ms (server {dry_server} ms), \
         commit {commit_ms} ms (server {commit_server} ms, 4 batches of 500), total {total_ms} ms",
        file.len()
    );
    // Generous ceiling so a regression to per-row round trips / quadratic work is caught.
    assert!(
        total_ms < 600_000,
        "2,000-student import took {total_ms} ms"
    );
}
