//! Part 2 · S4-ui-import — what the admin Data import screen needs from the API, against
//! PostgreSQL through the real composition root (body limits, idempotency, session auth).
//!
//! * `upload_route_accepts_files_above_the_default_body_limit` — `POST /api/v1/platform/imports`
//!   takes a 2.4 MiB file (≈ 3.2 MiB of base64 JSON, above `SUTRA_MAX_BODY_BYTES` = 2 MiB and the
//!   1 MiB idempotency buffer) with its required `Idempotency-Key`; a body above the import route
//!   limit is 413; every other route (also the other import routes) keeps the 2 MiB limit (413).
//! * `finance_checker_reads_only_the_imports_it_decides` — a holder of `platform.import.approve`
//!   without `platform.import.manage` (finance approver) lists and reads only the jobs that need
//!   a second approver (404 for the others), and still cannot run, report or upload (403);
//!   faculty cannot read at all (403).
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use support::*;
use uuid::Uuid;

const T_LIMIT: &str = "0192f5a0-7c3e-7000-8000-0000000f4501";
const T_READ: &str = "0192f5a0-7c3e-7000-8000-0000000f4502";

const IMP: &str = "/api/v1/platform/imports";
const ADMIN: &str = "admin@sutra.dev";
const FINANCE_APPROVER: &str = "finance.approver@sutra.dev";
const FACULTY: &str = "meera.shah@sutra.dev";

const STUDENT_HEADER: &str = "student_number,given_name,family_name,programme_code,notes";

/// Standard base64 (with padding).
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

/// A JSON request with an explicit `Content-Length` (what browsers and nginx send).
fn sized(who: &Login, method: &str, path: &str, body: String) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("content-length", body.len())
        .header("cookie", &who.cookie)
        .header("x-csrf-token", &who.csrf)
        .header("idempotency-key", Uuid::now_v7().to_string())
        .body(Body::from(body))
        .unwrap()
}

fn upload_body(entity: &str, institution: Uuid, file_name: &str, bytes: &[u8]) -> String {
    json!({
        "entity": entity,
        "institutionId": institution,
        "fileName": file_name,
        "contentBase64": b64(bytes),
    })
    .to_string()
}

fn id(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {v}"))
        .to_owned()
}

async fn institution(app: &axum::Router, admin: &Login, tenant: &str, name: &str) -> Uuid {
    b3::academic_setup(app, admin, tenant, name)
        .await
        .institution_id
}

#[tokio::test]
async fn upload_route_accepts_files_above_the_default_body_limit() {
    let (app, _owner) = app(T_LIMIT).await;
    let admin = login(&app, ADMIN).await;
    let inst = institution(&app, &admin, T_LIMIT, "Import limits").await;

    // 360 students with a 7,000-character unmapped `notes` column: a 2.4 MiB file (≈ 3.2 MiB as
    // base64 JSON).
    let prefix = Uuid::now_v7().simple().to_string()[20..].to_uppercase();
    let notes = "n".repeat(7000);
    let mut csv = format!("{STUDENT_HEADER}\n");
    for i in 0..360 {
        csv.push_str(&format!("UL{prefix}-{i},Asha,Large,BTECH-CSE,{notes}\n"));
    }
    let file = csv.into_bytes();
    assert!(file.len() > 2 * 1024 * 1024, "{}", file.len());
    let body = upload_body("students", inst, "large.csv", &file);
    assert!(body.len() > 3 * 1024 * 1024, "{}", body.len());
    let (s, _, job) = send(&app, sized(&admin, "POST", IMP, body)).await;
    assert_eq!(s, StatusCode::CREATED, "{job}");
    assert_eq!(job["fileBytes"].as_i64().unwrap(), file.len() as i64);
    assert_eq!(job["summary"]["rowsInFile"], 360);
    assert_eq!(job["mapping"]["student_number"], "student_number");
    let (s, _, read) = send(&app, get(&admin, &format!("{IMP}/{}", id(&job)))).await;
    assert_eq!(s, StatusCode::OK, "{read}");
    assert_eq!(read["fileName"], "large.csv");

    // Above the import route's limit (a file far beyond the engine's 8 MiB): 413, no job.
    let too_big = "A".repeat(12 * 1024 * 1024);
    let body = json!({
        "entity": "students",
        "institutionId": inst,
        "fileName": "huge.csv",
        "contentBase64": too_big,
    })
    .to_string();
    let (s, _, b) = send(&app, sized(&admin, "POST", IMP, body)).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{b}");
    let (s, _, list) = send(&app, get(&admin, &format!("{IMP}?institutionId={inst}"))).await;
    assert_eq!(s, StatusCode::OK);
    let names: Vec<&str> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|j| j["fileName"].as_str())
        .collect();
    assert_eq!(names, vec!["large.csv"], "{list}");

    // The larger limit is for the upload only: 3 MiB on another import route is 413.
    let profile = json!({
        "entity": "students",
        "name": "x".repeat(3 * 1024 * 1024),
        "mapping": {},
    })
    .to_string();
    let path = format!("{IMP}/mapping-profiles");
    let (s, _, _) = send(&app, sized(&admin, "POST", &path, profile)).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    let mapping = json!({"mapping": {"notes": "x".repeat(3 * 1024 * 1024)}}).to_string();
    let path = format!("{IMP}/{}/mapping", id(&job));
    let (s, _, _) = send(&app, sized(&admin, "PUT", &path, mapping)).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn finance_checker_reads_only_the_imports_it_decides() {
    let (app, _owner) = app(T_READ).await;
    let admin = login(&app, ADMIN).await;
    let approver = login(&app, FINANCE_APPROVER).await;
    let faculty = login(&app, FACULTY).await;
    let inst = institution(&app, &admin, T_READ, "Import readers").await;
    let prefix = Uuid::now_v7().simple().to_string()[20..].to_uppercase();

    let students = format!("{STUDENT_HEADER}\nRD{prefix}-1,Ravi,Reader,BTECH-CSE,\n");
    let body = upload_body("students", inst, "students.csv", students.as_bytes());
    let (s, _, st_job) = send(&app, sized(&admin, "POST", IMP, body)).await;
    assert_eq!(s, StatusCode::CREATED, "{st_job}");
    let balances = format!("student_number,amount\nRD{prefix}-1,1250.00\n");
    let body = upload_body(
        "opening_balances",
        inst,
        "balances.csv",
        balances.as_bytes(),
    );
    let (s, _, ob_job) = send(&app, sized(&admin, "POST", IMP, body)).await;
    assert_eq!(s, StatusCode::CREATED, "{ob_job}");
    assert_eq!(ob_job["requiresApproval"], true);
    let (st, ob) = (id(&st_job), id(&ob_job));

    // The administrator (manage) lists both.
    let (s, _, list) = send(&app, get(&admin, &format!("{IMP}?institutionId={inst}"))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 2, "{list}");

    // The finance checker lists and reads only the job that needs its approval.
    let (s, _, list) = send(&app, get(&approver, &format!("{IMP}?institutionId={inst}"))).await;
    assert_eq!(s, StatusCode::OK, "{list}");
    let ids: Vec<String> = list["items"].as_array().unwrap().iter().map(id).collect();
    assert_eq!(ids, vec![ob.clone()], "{list}");
    let (s, _, j) = send(&app, get(&approver, &format!("{IMP}/{ob}"))).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["createdBy"], "dev|admin@sutra.dev");
    let (s, _, rows) = send(&app, get(&approver, &format!("{IMP}/{ob}/rows"))).await;
    assert_eq!(s, StatusCode::OK, "{rows}");
    assert_eq!(rows["items"].as_array().unwrap().len(), 1);
    for path in [format!("{IMP}/{st}"), format!("{IMP}/{st}/rows")] {
        let (s, _, b) = send(&app, get(&approver, &path)).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{path}: {b}");
    }
    // Still no import work for the checker.
    let (s, _, _) = send(&app, get(&approver, &format!("{IMP}/{ob}/errors.csv"))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = send(&app, get(&approver, &format!("{IMP}/entities"))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let path = format!("{IMP}/{ob}/dry-run");
    let (s, _, _) = send(&app, sized(&approver, "POST", &path, "{}".into())).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let body = upload_body("opening_balances", inst, "mine.csv", balances.as_bytes());
    let (s, _, _) = send(&app, sized(&approver, "POST", IMP, body)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Faculty holds neither permission.
    for path in [
        format!("{IMP}?institutionId={inst}"),
        format!("{IMP}/{ob}"),
        format!("{IMP}/{ob}/rows"),
    ] {
        let (s, _, _) = send(&app, get(&faculty, &path)).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{path}");
    }
}
