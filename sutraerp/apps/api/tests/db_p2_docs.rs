//! Part 2 · Stage 4.2 (S4-docs) DB tests: document storage against PostgreSQL **and a real MinIO**
//! (S3-compatible, path-style, SigV4). CI starts MinIO in the `rust-db` job:
//!
//! ```sh
//! SUTRA_TEST_S3_ENDPOINT=http://127.0.0.1:9000 \
//!   cargo test -p sutra-api --features db-tests,s3-tests --test db_p2_docs
//! ```
//!
//! * upload → download round trip through the API (sanitised name, detected type, SHA-256,
//!   attachment headers), one file per document, presigned link after authorisation;
//! * cross-tenant and cross-institution denial through the API, guessed ids, and direct key
//!   guessing against the bucket (anonymous access refused);
//! * a tampered or missing object is detected on read and on verification, and quarantined;
//! * oversize and forbidden types are rejected (magic bytes, not the extension); the JSON body
//!   limit is not widened by the upload limit;
//! * retention and legal hold are enforced (API, database trigger) and the sweeper expires and
//!   purges only what is allowed;
//! * audit rows for upload, download, link, integrity failure, delete, hold and expiry;
//! * the admission flow: staff upload + verify, the created student retrieves the verified files.
#![cfg(all(feature = "db-tests", feature = "s3-tests"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::time::Duration;
use support::*;
use sutra_api::app::{router, AppContext};
use sutra_api::config::Config;
use sutra_infra::s3::{S3Client, S3Settings, ServerSideEncryption};
use sutra_kernel::secret::{hex, sha256};
use sutra_persistence::jobs::SweepJob;
use tower::ServiceExt;
use uuid::Uuid;

const API: &str = "/api/v1/student";
const BUCKET: &str = "sutra-it-docs";
const PDF: &[u8] = b"%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF\n";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0";

fn tenant() -> &'static str {
    Box::leak(Uuid::now_v7().to_string().into_boxed_str())
}

fn s3_endpoint() -> String {
    std::env::var("SUTRA_TEST_S3_ENDPOINT").unwrap_or_else(|_| "http://127.0.0.1:9000".into())
}

fn s3_key() -> String {
    std::env::var("SUTRA_TEST_S3_ACCESS_KEY").unwrap_or_else(|_| "sutraci".into())
}

fn s3_secret() -> String {
    std::env::var("SUTRA_TEST_S3_SECRET_KEY").unwrap_or_else(|_| "sutraci-minio-secret".into())
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn runtime_url() -> &'static str {
    let url = database_url();
    let (scheme, rest) = url.split_once("://").expect("postgres URL");
    let host = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    leak(format!(
        "{scheme}://{RUNTIME_USER}:{RUNTIME_PASSWORD}@{host}"
    ))
}

/// Postgres + MinIO backed app in `tenant` (dev auth; `extra` overrides).
async fn docs_app(tenant: &'static str, extra: &[(&'static str, &'static str)]) -> (Router, PgPool) {
    let owner = owner_pool().await;
    let mut src: HashMap<&'static str, &'static str> = HashMap::from([
        ("SUTRA_ENV", "development"),
        ("DATABASE_URL", runtime_url()),
        ("SUTRA_SESSION_STORE", "postgres"),
        ("SUTRA_IDEMPOTENCY_STORE", "postgres"),
        ("SUTRA_AUDIT_SINK", "postgres"),
        ("SUTRA_DEV_TENANT_ID", tenant),
        ("SUTRA_SWEEPERS", "false"),
        ("SUTRA_INBOX_CONSUMER", "false"),
        ("SUTRA_APPROVAL_MODE", "auto"),
        ("SUTRA_DOCUMENT_STORAGE", "s3"),
        ("SUTRA_S3_ENDPOINT", leak(s3_endpoint())),
        ("SUTRA_S3_BUCKET", BUCKET),
        ("SUTRA_S3_ACCESS_KEY_ID", leak(s3_key())),
        ("SUTRA_S3_SECRET_ACCESS_KEY", leak(s3_secret())),
        ("SUTRA_S3_CREATE_BUCKET", "true"),
        ("SUTRA_DOCS_PRESIGN_TTL_SECS", "60"),
    ]);
    src.extend(extra.iter().copied());
    let cfg = Config::load(&src).expect("config");
    let ctx = AppContext::from_config(&cfg).expect("context");
    (router(ctx), owner)
}

/// Direct bucket client (operations / tampering in tests).
fn bucket() -> S3Client {
    S3Client::new(S3Settings {
        endpoint: s3_endpoint(),
        public_endpoint: None,
        region: "us-east-1".into(),
        bucket: BUCKET.into(),
        access_key_id: s3_key(),
        secret_access_key: s3_secret(),
        path_style: true,
        sse: ServerSideEncryption::None,
        sse_kms_key_id: None,
        create_bucket: true,
        timeout: Duration::from_secs(10),
    })
    .unwrap()
}

/// Anonymous GET straight at the bucket (no credentials): the HTTP status.
fn anonymous_status(path: &str) -> u16 {
    let ep = s3_endpoint();
    let hostport = ep.trim_start_matches("http://").trim_end_matches('/');
    let mut s = std::net::TcpStream::connect(hostport).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    buf.split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0)
}

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap_or_else(|| panic!("no id in {v}")).to_owned()
}

fn ver(v: &Value) -> i64 {
    v["version"].as_i64().unwrap_or_else(|| panic!("no version in {v}"))
}

async fn call(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
) -> (StatusCode, Value) {
    let req = if method == "GET" {
        get(who, path)
    } else {
        write(who, method, path, body, if_match)
    };
    let (s, _, b) = send(app, req).await;
    (s, b)
}

async fn ok(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
    expect: StatusCode,
) -> Value {
    let (s, b) = call(app, who, method, path, body, if_match).await;
    assert_eq!(s, expect, "{method} {path}: {b}");
    b
}

/// Raw file upload (no Idempotency-Key: a document takes exactly one file).
async fn upload(
    app: &Router,
    who: &Login,
    path: &str,
    filename: &str,
    content_type: &str,
    bytes: Vec<u8>,
) -> (StatusCode, Value) {
    let q: String = url_encode(filename);
    let req = Request::put(format!("{path}?filename={q}"))
        .header("content-type", content_type)
        .header("cookie", &who.cookie)
        .header("x-csrf-token", &who.csrf)
        .body(Body::from(bytes))
        .unwrap();
    let (s, _, b) = send(app, req).await;
    (s, b)
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// GET returning the raw bytes.
async fn download(app: &Router, who: &Login, path: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
    let res = app.clone().oneshot(get(who, path)).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, headers, bytes)
}

async fn audit_actions(pool: &PgPool, target: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT action FROM platform.audit_log WHERE target_id = $1 ORDER BY occurred_at, id",
    )
    .bind(target)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn storage_key(pool: &PgPool, doc: &str) -> String {
    sqlx::query_scalar("SELECT storage_key FROM platform.document_object WHERE id = $1::uuid")
        .bind(doc)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Draft application → offer → accepted admission (`initiated`) in `setup`'s institution.
async fn accepted_admission(
    app: &Router,
    admin: &Login,
    setup: &b3::AcademicSetup,
    given: &str,
    subject: Option<&str>,
) -> Value {
    let inst = setup.institution_id;
    let a = ok(
        app,
        admin,
        "POST",
        &format!("{API}/institutions/{inst}/admission-applications"),
        json!({
            "programmeId": setup.programme_id, "givenName": given, "familyName": "Joshi",
            "dateOfBirth": "2007-03-14", "email": format!("{}@example.in", given.to_lowercase()),
            "phone": "+91 98765 43210", "subjectRef": subject,
            "admissionTermId": setup.term_ids[0],
        }),
        None,
        StatusCode::CREATED,
    )
    .await;
    let mut a = a;
    for step in ["submit", "review"] {
        a = ok(
            app,
            admin,
            "POST",
            &format!("{API}/admission-applications/{}/{step}", id(&a)),
            json!({}),
            Some(ver(&a)),
            StatusCode::OK,
        )
        .await;
    }
    let a = ok(
        app,
        admin,
        "POST",
        &format!("{API}/admission-applications/{}/offer", id(&a)),
        json!({}),
        Some(ver(&a)),
        StatusCode::OK,
    )
    .await;
    let aid = a["admissionId"].as_str().unwrap().to_owned();
    let adm = ok(
        app,
        admin,
        "GET",
        &format!("{API}/admissions/{aid}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    ok(
        app,
        admin,
        "POST",
        &format!("{API}/admissions/{aid}/accept"),
        json!({}),
        Some(ver(&adm)),
        StatusCode::OK,
    )
    .await
}

async fn add_document(app: &Router, admin: &Login, aid: &str, doc_type: &str) -> Value {
    ok(
        app,
        admin,
        "POST",
        &format!("{API}/admissions/{aid}/documents"),
        json!({"documentType": doc_type, "title": format!("{doc_type} scan"), "fileRef": "upload"}),
        None,
        StatusCode::CREATED,
    )
    .await
}

fn file_path(aid: &str, did: &str) -> String {
    format!("{API}/admissions/{aid}/documents/{did}/file")
}

// ------------------------------------------------------------------------------- tests

/// Round trip, one file per document, attachment headers, permissions, presigned link and the
/// audit trail; the bytes really are in the private bucket under a tenant-rooted key.
#[tokio::test]
async fn upload_download_round_trip_and_audit() {
    let t = tenant();
    let (app, pool) = docs_app(t, &[]).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let faculty = login(&app, "meera.shah@sutra.dev").await;
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, t, "Docs University").await;
    let adm = accepted_admission(&app, &admin, &setup, "Ishaan", None).await;
    let aid = id(&adm);
    let doc = add_document(&app, &admin, &aid, "identity_proof").await;
    let did = id(&doc);
    let path = file_path(&aid, &did);

    // Nothing to download before a file is attached.
    let (s, _, _) = download(&app, &admin, &path).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Only admission staff may attach.
    let (s, _) = upload(&app, &faculty, &path, "a.pdf", "application/pdf", PDF.to_vec()).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let (s, b) = upload(
        &app,
        &admin,
        &path,
        "../../Aadhaar card.pdf",
        "application/pdf",
        PDF.to_vec(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let file = &b["file"];
    let object = file["id"].as_str().unwrap().to_owned();
    assert_eq!(file["filename"], "Aadhaar card.pdf", "path stripped");
    assert_eq!(file["contentType"], "application/pdf");
    assert_eq!(file["sizeBytes"], PDF.len());
    assert_eq!(file["sha256"], hex(&sha256(PDF)));
    assert_eq!(file["ownerType"], "admission_document");
    assert_eq!(file["ownerId"], did.as_str());
    assert_eq!(file["institutionId"], json!(setup.institution_id));
    assert!(b.get("storageKey").is_none() && file.get("storageKey").is_none());
    assert_eq!(b["documentVersion"], ver(&doc) + 1);

    // One file per document: a retry / second upload is refused and the first file stands.
    let (s, _) = upload(&app, &admin, &path, "b.pdf", "application/pdf", PDF.to_vec()).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let docs = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/admissions/{aid}/documents"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert!(docs.to_string().contains(&object), "{docs}");

    // Download: identical bytes, never rendered inline.
    let (s, h, bytes) = download(&app, &admin, &path).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(bytes, PDF);
    assert_eq!(h[header::CONTENT_TYPE], "application/pdf");
    assert_eq!(
        h[header::CONTENT_DISPOSITION],
        "attachment; filename=\"Aadhaar card.pdf\""
    );
    assert_eq!(h["x-document-sha256"], hex(&sha256(PDF)).as_str());
    assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(h[header::CACHE_CONTROL], "no-store");
    // Faculty (no admission rights) and the applicant-to-be student cannot read it.
    for who in [&faculty, &student] {
        let (s, _, _) = download(&app, who, &path).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }

    // Presigned link only after authorisation; short-lived; signed.
    let (s, link) = call(&app, &admin, "POST", &format!("{path}/link"), json!({}), None).await;
    assert_eq!(s, StatusCode::OK, "{link}");
    let url = link["url"].as_str().unwrap();
    assert!(url.contains("X-Amz-Signature=") && url.contains("X-Amz-Expires=60"), "{url}");
    let (s, _) = call(&app, &faculty, "POST", &format!("{path}/link"), json!({}), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Tenant-rooted key, private bucket: the object is there, anonymous access is refused.
    let key = storage_key(&pool, &object).await;
    assert!(
        key.starts_with(&format!(
            "{t}/{}/admission_document/{did}/",
            setup.institution_id
        )),
        "{key}"
    );
    assert_eq!(bucket().get_object(&key, 1 << 20).await.unwrap(), PDF);
    assert_eq!(anonymous_status(&format!("/{BUCKET}/{key}")), 403);
    assert_eq!(anonymous_status(&format!("/{BUCKET}?list-type=2")), 403);

    // Records management can read the metadata; others cannot.
    let meta = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/v1/platform/documents/{object}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(meta["sha256"], hex(&sha256(PDF)));
    assert!(!meta["legalHold"].as_bool().unwrap());
    let (s, _) = call(
        &app,
        &faculty,
        "GET",
        &format!("/api/v1/platform/documents/{object}"),
        json!(null),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let actions = audit_actions(&pool, &object).await;
    for a in [
        "platform.document.upload",
        "platform.document.download",
        "platform.document.link",
    ] {
        assert!(actions.iter().any(|x| x == a), "{a} missing in {actions:?}");
    }
    let attach: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.audit_log WHERE action = \
         'student.admission.attach_document_file' AND target_id = $1",
    )
    .bind(&aid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(attach, 1);
}

/// Other tenants, other institutions, guessed ids and guessed storage keys never reach a file.
#[tokio::test]
async fn cross_tenant_and_cross_institution_access_is_denied() {
    let t = tenant();
    let (app, pool) = docs_app(t, &[]).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, t, "Docs Tenant A").await;
    let adm = accepted_admission(&app, &admin, &setup, "Kabir", None).await;
    let aid = id(&adm);
    let doc = add_document(&app, &admin, &aid, "identity_proof").await;
    let did = id(&doc);
    let other = add_document(&app, &admin, &aid, "qualifying_marksheet").await;
    let path = file_path(&aid, &did);
    let (s, b) = upload(&app, &admin, &path, "id.pdf", "application/pdf", PDF.to_vec()).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let object = b["file"]["id"].as_str().unwrap().to_owned();

    // Another tenant: the same ids are simply not there (API and platform metadata).
    let t2 = tenant();
    let (app2, _) = docs_app(t2, &[]).await;
    let admin2 = login(&app2, "admin@sutra.dev").await;
    let (s, _, _) = download(&app2, &admin2, &path).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(&app2, &admin2, "POST", &format!("{path}/link"), json!({}), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &app2,
        &admin2,
        "GET",
        &format!("/api/v1/platform/documents/{object}"),
        json!(null),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &app2,
        &admin2,
        "POST",
        &format!("/api/v1/platform/documents/{object}/legal-hold"),
        json!({"hold": true, "reason": "probe"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Same tenant, session pinned to another institution: not visible either.
    let inst_b = register_institution(&app, &admin, "Docs Institute B").await;
    let (app_b, _) = docs_app(t, &[("SUTRA_DEV_INSTITUTION_ID", leak(inst_b))]).await;
    let admin_b = login(&app_b, "admin@sutra.dev").await;
    let (s, _, _) = download(&app_b, &admin_b, &path).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &app_b,
        &admin_b,
        "GET",
        &format!("/api/v1/platform/documents/{object}"),
        json!(null),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Guessed ids in the right tenant: another document of the same admission (no file), the
    // file of document A under document B's path, random ids.
    let (s, _, _) = download(&app, &admin, &file_path(&aid, &id(&other))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = download(&app, &admin, &file_path(&aid, &Uuid::now_v7().to_string())).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let other_adm = accepted_admission(&app, &admin, &setup, "Vihaan", None).await;
    let (s, _, _) = download(&app, &admin, &file_path(&id(&other_adm), &did)).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "document of another admission");
    // The database refuses to swap the file of a document to someone else's object.
    let swapped = sqlx::query(
        "UPDATE student.admission_document SET document_object_id = $2::uuid WHERE id = $1::uuid",
    )
    .bind(&did)
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await;
    assert!(swapped
        .unwrap_err()
        .to_string()
        .contains("cannot be replaced or removed"));

    // Direct key guessing at the bucket: refused without credentials (403), whatever the key.
    let key = storage_key(&pool, &object).await;
    assert_eq!(anonymous_status(&format!("/{BUCKET}/{key}")), 403);
    assert_eq!(
        anonymous_status(&format!(
            "/{BUCKET}/{t}/{}/admission_document/{did}/{object}",
            setup.institution_id
        )),
        403
    );
    // Row-level security: the runtime role sees no other tenant's metadata.
    let rt = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(runtime_url())
        .await
        .unwrap();
    let mut c = rt.acquire().await.unwrap();
    sqlx::query("SELECT set_config('sutra.tenant_id', $1, false)")
        .bind(t2)
        .execute(&mut *c)
        .await
        .unwrap();
    let seen: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.document_object WHERE id = $1::uuid",
    )
    .bind(&object)
    .fetch_one(&mut *c)
    .await
    .unwrap();
    assert_eq!(seen, 0, "RLS hides other tenants' documents");
}

/// A changed or missing object is detected on download and on verification, quarantined and
/// audited; nothing is served afterwards.
#[tokio::test]
async fn tampered_object_is_detected_and_quarantined() {
    let t = tenant();
    let (app, pool) = docs_app(t, &[]).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, t, "Docs Integrity").await;
    let adm = accepted_admission(&app, &admin, &setup, "Reyansh", None).await;
    let aid = id(&adm);
    let doc = add_document(&app, &admin, &aid, "identity_proof").await;
    let did = id(&doc);
    let path = file_path(&aid, &did);
    let (s, b) = upload(&app, &admin, &path, "id.pdf", "application/pdf", PDF.to_vec()).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let object = b["file"]["id"].as_str().unwrap().to_owned();
    let version = b["documentVersion"].as_i64().unwrap();
    let key = storage_key(&pool, &object).await;

    // Someone with bucket credentials replaces the bytes (same size, different content).
    let mut tampered = PDF.to_vec();
    let n = tampered.len() - 8;
    tampered[n] ^= 0x20;
    bucket()
        .put_object(&key, "application/pdf", tampered.clone(), &sha256(&tampered))
        .await
        .unwrap();
    let (s, _, body) = download(&app, &admin, &path).await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", String::from_utf8_lossy(&body));
    assert!(String::from_utf8_lossy(&body).contains("integrity"));
    let q: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT quarantined_at FROM platform.document_object WHERE id = $1::uuid",
    )
    .bind(&object)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(q.is_some(), "quarantined");
    assert!(audit_actions(&pool, &object)
        .await
        .contains(&"platform.document.integrity_failed".to_owned()));
    // Quarantine sticks: restoring the bytes does not silently re-open the file, verification
    // is refused, and the stamp cannot be cleared by the runtime.
    bucket()
        .put_object(&key, "application/pdf", PDF.to_vec(), &sha256(PDF))
        .await
        .unwrap();
    let (s, _, _) = download(&app, &admin, &path).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/documents/{did}/verify"),
        json!({"outcome": "verified"}),
        Some(version),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    let cleared = sqlx::query(
        "UPDATE platform.document_object SET quarantined_at = NULL WHERE id = $1::uuid",
    )
    .bind(&object)
    .execute(&pool)
    .await;
    assert!(cleared.is_err(), "quarantine stamp is permanent");

    // A missing object is an integrity failure too (verification step).
    let doc2 = add_document(&app, &admin, &aid, "qualifying_marksheet").await;
    let did2 = id(&doc2);
    let (s, b) = upload(
        &app,
        &admin,
        &file_path(&aid, &did2),
        "marks.png",
        "image/png",
        PNG.to_vec(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let object2 = b["file"]["id"].as_str().unwrap().to_owned();
    let v2 = b["documentVersion"].as_i64().unwrap();
    bucket()
        .delete_object(&storage_key(&pool, &object2).await)
        .await
        .unwrap();
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/documents/{did2}/verify"),
        json!({"outcome": "verified"}),
        Some(v2),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    let status: String =
        sqlx::query_scalar("SELECT status FROM student.admission_document WHERE id = $1::uuid")
            .bind(&did2)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "submitted", "not verified against a missing file");
    assert!(audit_actions(&pool, &object2)
        .await
        .contains(&"platform.document.integrity_failed".to_owned()));
}

/// Size limit, allow-listed types by content, sanitised names; nothing stored when refused;
/// the 2 MiB JSON limit still holds on every other route.
#[tokio::test]
async fn oversize_and_forbidden_types_are_rejected() {
    let t = tenant();
    let (app, pool) = docs_app(t, &[("SUTRA_DOCS_MAX_BYTES", "4096")]).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, t, "Docs Validation").await;
    let adm = accepted_admission(&app, &admin, &setup, "Arjun", None).await;
    let aid = id(&adm);
    let doc = add_document(&app, &admin, &aid, "identity_proof").await;
    let did = id(&doc);
    let path = file_path(&aid, &did);

    let mut big = PDF.to_vec();
    big.resize(4097, b' ');
    let cases: Vec<(&str, &str, Vec<u8>, &str)> = vec![
        ("big.pdf", "application/pdf", big, "too_large"),
        ("empty.pdf", "application/pdf", vec![], "empty"),
        // Content decides, not the extension or the declared type.
        ("setup.pdf", "application/pdf", b"MZ\x90\0\x03\0\0\0".to_vec(), "type_not_allowed"),
        ("page.pdf", "application/pdf", b"<html><script>alert(1)</script>".to_vec(), "type_not_allowed"),
        ("doc.pdf", "application/pdf", b"PK\x03\x04docx".to_vec(), "type_not_allowed"),
        ("scan.png", "image/png", PDF.to_vec(), "extension_mismatch"),
        ("scan.pdf", "image/png", PDF.to_vec(), "type_mismatch"),
        (
            "macro.pdf",
            "application/pdf",
            b"%PDF-1.4 << /OpenAction << /S /JavaScript /JS (app.alert(1)) >> >>".to_vec(),
            "active_content",
        ),
        ("..", "application/pdf", PDF.to_vec(), "invalid"),
    ];
    for (name, ct, bytes, code) in cases {
        let (s, b) = upload(&app, &admin, &path, name, ct, bytes).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{name}: {b}");
        assert!(b.to_string().contains(code), "{name}: expected {code} in {b}");
    }
    // Above the transport limit for uploads: refused before the handler (413).
    let (s, _) = upload(
        &app,
        &admin,
        &path,
        "huge.pdf",
        "application/pdf",
        vec![b'%'; 3 * 1024 * 1024],
    )
    .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    let stored: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.document_object WHERE owner_id = $1::uuid",
    )
    .bind(&did)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored, 0, "nothing stored for refused uploads");
    // A valid file still goes through afterwards.
    let (s, b) = upload(&app, &admin, &path, "ok.pdf", "application/pdf", PDF.to_vec()).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");

    // The upload limit is for `PUT …/file` only: a 3 MiB JSON body anywhere else is 413.
    let req = Request::post(format!("{API}/admissions/{aid}/documents"))
        .header("content-type", "application/json")
        .header("cookie", &admin.cookie)
        .header("x-csrf-token", &admin.csrf)
        .body(Body::from(format!(
            "{{\"documentType\":\"x\",\"title\":\"{}\"}}",
            "a".repeat(3 * 1024 * 1024)
        )))
        .unwrap();
    let (s, _, _) = send(&app, req).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
}

/// Retention and legal hold: the API and the database both refuse early deletion and deletion
/// under hold; deletion removes the bytes; the sweeper expires and purges only what is allowed.
#[tokio::test]
async fn retention_and_legal_hold_are_enforced() {
    // Default retention (8 years): deletion refused, audited as rejected.
    let t = tenant();
    let (app, pool) = docs_app(t, &[]).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, t, "Docs Retention").await;
    let adm = accepted_admission(&app, &admin, &setup, "Advik", None).await;
    let aid = id(&adm);
    let doc = add_document(&app, &admin, &aid, "identity_proof").await;
    let (s, b) = upload(
        &app,
        &admin,
        &file_path(&aid, &id(&doc)),
        "id.pdf",
        "application/pdf",
        PDF.to_vec(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let retained = b["file"]["id"].as_str().unwrap().to_owned();
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/platform/documents/{retained}/delete"),
        json!({"reason": "cleanup"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert!(b.to_string().contains("retention"), "{b}");
    let early = sqlx::query(
        "UPDATE platform.document_object SET deleted_at = now() WHERE id = $1::uuid",
    )
    .bind(&retained)
    .execute(&pool)
    .await;
    assert!(early.unwrap_err().to_string().contains("under retention"));
    let shorten = sqlx::query(
        "UPDATE platform.document_object SET retention_until = retention_until - interval '1 day' \
         WHERE id = $1::uuid",
    )
    .bind(&retained)
    .execute(&pool)
    .await;
    assert!(shorten.unwrap_err().to_string().contains("only be extended"));
    let gone = sqlx::query("DELETE FROM platform.document_object WHERE id = $1::uuid")
        .bind(&retained)
        .execute(&pool)
        .await;
    assert!(gone.unwrap_err().to_string().contains("never deleted"));

    // Retention 0 (development only): legal hold blocks, release then delete works.
    let (app0, _) = docs_app(t, &[("SUTRA_DOCS_RETENTION_DAYS", "0")]).await;
    let admin0 = login(&app0, "admin@sutra.dev").await;
    let doc_h = add_document(&app0, &admin0, &aid, "qualifying_marksheet").await;
    let (s, b) = upload(
        &app0,
        &admin0,
        &file_path(&aid, &id(&doc_h)),
        "marks.png",
        "image/png",
        PNG.to_vec(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let held = b["file"]["id"].as_str().unwrap().to_owned();
    let docs = format!("/api/v1/platform/documents/{held}");
    let (s, b) = call(&app0, &admin0, "POST", &format!("{docs}/legal-hold"), json!({"hold": true, "reason": " "}), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "reason required: {b}");
    let b = ok(
        &app0,
        &admin0,
        "POST",
        &format!("{docs}/legal-hold"),
        json!({"hold": true, "reason": "RTI appeal 42/2026"}),
        None,
        StatusCode::OK,
    )
    .await;
    assert!(b["legalHold"].as_bool().unwrap());
    let (s, b) = call(&app0, &admin0, "POST", &format!("{docs}/delete"), json!({"reason": "x"}), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert!(b.to_string().contains("legal hold"), "{b}");
    let direct = sqlx::query(
        "UPDATE platform.document_object SET deleted_at = now() WHERE id = $1::uuid",
    )
    .bind(&held)
    .execute(&pool)
    .await;
    assert!(direct.unwrap_err().to_string().contains("legal hold"));
    // The sweeper never touches a held document.
    let sweep = sutra_platform::documents::DocumentRetentionSweep {
        store: std::sync::Arc::new(sutra_api::storage::S3ObjectStore(std::sync::Arc::new(
            bucket(),
        ))),
        interval: Duration::from_secs(60),
        batch_limit: 500,
    };
    let mut conn = pool.acquire().await.unwrap();
    sweep.run(&mut conn).await.unwrap();
    let deleted: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT deleted_at FROM platform.document_object WHERE id = $1::uuid",
    )
    .bind(&held)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(deleted.is_none(), "held document survives the sweep");
    ok(
        &app0,
        &admin0,
        "POST",
        &format!("{docs}/legal-hold"),
        json!({"hold": false, "reason": "appeal closed"}),
        None,
        StatusCode::OK,
    )
    .await;
    let key = storage_key(&pool, &held).await;
    let b = ok(
        &app0,
        &admin0,
        "POST",
        &format!("{docs}/delete"),
        json!({"reason": "duplicate scan"}),
        None,
        StatusCode::OK,
    )
    .await;
    assert!(b["deletedAt"].is_string());
    assert!(matches!(
        bucket().get_object(&key, 1 << 20).await,
        Err(sutra_kernel::error::KernelError::NotFound { .. })
    ));
    let (s, _, _) = download(&app0, &admin0, &file_path(&aid, &id(&doc_h))).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "a deleted document is gone");
    let purged: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT purged_at FROM platform.document_object WHERE id = $1::uuid",
    )
    .bind(&held)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(purged.is_some());
    let actions = audit_actions(&pool, &held).await;
    for a in [
        "platform.document.legal_hold.place",
        "platform.document.legal_hold.release",
        "platform.document.delete",
    ] {
        assert!(actions.iter().any(|x| x == a), "{a} missing in {actions:?}");
    }
    let rejected: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.audit_log WHERE target_id = $1 \
         AND action = 'platform.document.delete' AND outcome = 'rejected'",
    )
    .bind(&held)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rejected, 1, "refused deletion audited");

    // Expiry: a document whose retention ended is deleted + purged by the sweeper (audited);
    // the 8-year document is untouched.
    let doc_e = add_document(&app0, &admin0, &aid, "transfer_certificate").await;
    let (s, b) = upload(
        &app0,
        &admin0,
        &file_path(&aid, &id(&doc_e)),
        "tc.pdf",
        "application/pdf",
        PDF.to_vec(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let expiring = b["file"]["id"].as_str().unwrap().to_owned();
    let ekey = storage_key(&pool, &expiring).await;
    let n = sweep.run(&mut conn).await.unwrap();
    assert!(n >= 2, "expired + purged: {n}");
    let (d, p): (
        Option<chrono::DateTime<chrono::Utc>>,
        Option<chrono::DateTime<chrono::Utc>>,
    ) = sqlx::query_as(
        "SELECT deleted_at, purged_at FROM platform.document_object WHERE id = $1::uuid",
    )
    .bind(&expiring)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(d.is_some() && p.is_some());
    assert!(bucket().get_object(&ekey, 1 << 20).await.is_err());
    assert!(audit_actions(&pool, &expiring)
        .await
        .contains(&"platform.document.expire".to_owned()));
    let kept: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT deleted_at FROM platform.document_object WHERE id = $1::uuid",
    )
    .bind(&retained)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(kept.is_none(), "retained document untouched by the sweeper");
}

/// The admission flow end to end: staff attach and verify the files (SHA-256 re-checked), the
/// created student retrieves the verified documents on his / her own record — nobody else's.
#[tokio::test]
async fn student_retrieves_verified_admission_documents() {
    let t = tenant();
    let (app, pool) = docs_app(t, &[]).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let faculty = login(&app, "meera.shah@sutra.dev").await;
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, t, "Docs Admissions").await;
    let adm = accepted_admission(
        &app,
        &admin,
        &setup,
        "Aarav",
        Some("dev|aarav.desai@sutra.dev"),
    )
    .await;
    let aid = id(&adm);
    let mut files = vec![];
    for (i, ty) in adm["requiredDocuments"]
        .as_array()
        .unwrap()
        .clone()
        .iter()
        .enumerate()
    {
        let ty = ty.as_str().unwrap();
        let d = add_document(&app, &admin, &aid, ty).await;
        let bytes = if i == 0 { PDF.to_vec() } else { PNG.to_vec() };
        let (name, ct) = if i == 0 {
            ("identity.pdf", "application/pdf")
        } else {
            ("marks.png", "image/png")
        };
        let (s, b) = upload(&app, &admin, &file_path(&aid, &id(&d)), name, ct, bytes.clone()).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        files.push((id(&d), b["documentVersion"].as_i64().unwrap(), bytes));
    }
    let adm = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/admissions/{aid}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    let mut adm = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/submit-documents"),
        json!({}),
        Some(ver(&adm)),
        StatusCode::OK,
    )
    .await;
    for (did, v, _) in &files {
        ok(
            &app,
            &admin,
            "POST",
            &format!("{API}/admissions/{aid}/documents/{did}/verify"),
            json!({"outcome": "verified"}),
            Some(*v),
            StatusCode::OK,
        )
        .await;
    }
    for step in [
        "submit-for-approval",
        "financial-clearance",
        "confirm",
        "create-student",
    ] {
        adm = ok(
            &app,
            &admin,
            "GET",
            &format!("{API}/admissions/{aid}"),
            json!(null),
            None,
            StatusCode::OK,
        )
        .await;
        adm = ok(
            &app,
            &admin,
            "POST",
            &format!("{API}/admissions/{aid}/{step}"),
            json!({}),
            Some(ver(&adm)),
            StatusCode::OK,
        )
        .await;
    }
    assert_eq!(adm["status"], "student_created", "{adm}");
    let sid = adm["studentId"].as_str().unwrap().to_owned();
    let verified: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.audit_log WHERE action = \
         'platform.document.verify_integrity' AND details->>'ownerType' = 'admission_document' \
         AND tenant_id = $1::uuid",
    )
    .bind(t)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(verified, files.len() as i64, "every verification re-checked the bytes");

    // The student's own record carries the verified files.
    let mine = ok(
        &app,
        &student,
        "GET",
        &format!("{API}/me/documents"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    let items: Vec<Value> = mine
        .as_array()
        .cloned()
        .or_else(|| mine["items"].as_array().cloned())
        .unwrap_or_default();
    let with_file: Vec<&Value> = items
        .iter()
        .filter(|d| d["documentObjectId"].is_string())
        .collect();
    assert_eq!(with_file.len(), files.len(), "{mine}");
    for d in &with_file {
        let sdid = id(d);
        let (s, h, bytes) = download(&app, &student, &format!("{API}/me/documents/{sdid}/file")).await;
        assert_eq!(s, StatusCode::OK);
        assert!(files.iter().any(|(_, _, b)| *b == bytes));
        assert!(h[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("attachment"));
        let (s, _, bytes2) =
            download(&app, &admin, &format!("{API}/students/{sid}/documents/{sdid}/file")).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(bytes, bytes2);
        let (s, _, _) =
            download(&app, &faculty, &format!("{API}/students/{sid}/documents/{sdid}/file")).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, b) = call(
            &app,
            &student,
            "POST",
            &format!("{API}/me/documents/{sdid}/file/link"),
            json!({}),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{b}");
    }

    // Another student's document is invisible to this student (guessed id).
    let other = b3::seed_student(&pool, &setup, "Other", None).await;
    let other_doc = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{}/documents", other.student_id),
        json!({"documentType": "identity_proof", "title": "Other ID", "fileRef": "upload"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let odid = id(&other_doc);
    let (s, b) = upload(
        &app,
        &admin,
        &format!("{API}/students/{}/documents/{odid}/file", other.student_id),
        "other.pdf",
        "application/pdf",
        PDF.to_vec(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let (s, _, _) = download(&app, &student, &format!("{API}/me/documents/{odid}/file")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &app,
        &student,
        "POST",
        &format!("{API}/me/documents/{odid}/file/link"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Staff with record rights can.
    let (s, _, bytes) = download(
        &app,
        &admin,
        &format!("{API}/students/{}/documents/{odid}/file", other.student_id),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(bytes, PDF);
}

/// Development without object storage: the file endpoints answer 503, everything else works.
#[tokio::test]
async fn disabled_storage_answers_503() {
    let t = tenant();
    let (app, _) = docs_app(t, &[("SUTRA_DOCUMENT_STORAGE", "disabled")]).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, t, "Docs Disabled").await;
    let adm = accepted_admission(&app, &admin, &setup, "Dev", None).await;
    let aid = id(&adm);
    let doc = add_document(&app, &admin, &aid, "identity_proof").await;
    let (s, _) = upload(
        &app,
        &admin,
        &file_path(&aid, &id(&doc)),
        "a.pdf",
        "application/pdf",
        PDF.to_vec(),
    )
    .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
}
