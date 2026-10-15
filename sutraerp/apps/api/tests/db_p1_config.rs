//! Part 1 · Stage 3 — configuration, transport and supplier bank-detail controls (S3-config).
//!
//! * `supplier_bank_details_change_only_through_an_independent_approval` (DEF-J-004) — the
//!   attack: an administrator (procurement maker) redirects an active supplier's payments by
//!   patching its bank account. Before the fix the PATCH overwrote the account at once, with an
//!   audit entry naming only the supplier number. Now the PATCH is refused (409), a direct SQL
//!   update is refused by the database trigger, and the only path is a bank-change request:
//!   pending (live details unchanged, approvers notified through `supplier.bank_change_requested`,
//!   audit with masked accounts), one open request at a time, its maker deciding it → 403, a user
//!   without an approve permission → 403, a procurement approver approves → the new account is
//!   live, recorded (status history keyed by the request, audit with masked old / new and the
//!   maker, `supplier.bank_change_approved`); a finance approver rejects a second request → the
//!   account stays. The full account number never appears in the audit log or the outbox.
//! * `api_transport_hardening_end_to_end` — the real composition root with the transport
//!   settings: hardening headers + HSTS on every answer (404 / 413 / 429 included), CORS only for
//!   the configured origin, request bodies above `SUTRA_MAX_BODY_BYTES` → 413, password guessing
//!   → 429 with `Retry-After` after the credential burst, `/api/openapi.json` not served when
//!   `SUTRA_API_DOCS=false` (the production rule), readiness without dependency error texts.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use std::collections::HashMap;
use support::b8;
use support::*;
use sutra_api::app::{router, AppContext};
use sutra_api::config::Config;
use sutra_kernel::approval::ApprovalMode;

const T_BANK: &str = "0192f5a0-7c3e-7000-8000-0000000c0f01";
const T_HTTP: &str = "0192f5a0-7c3e-7000-8000-0000000c0f02";

const R: &str = "/api/v1/resources";

const ADMIN: &str = "admin@sutra.dev";
const FACULTY: &str = "meera.shah@sutra.dev";
const PROCUREMENT_APPROVER: &str = "procurement.approver@sutra.dev";
const FINANCE_APPROVER: &str = "finance.approver@sutra.dev";

const OLD_ACCOUNT: &str = "001234567890";
const NEW_ACCOUNT: &str = "998877665544";
const SECOND_ACCOUNT: &str = "112233445566";

fn runtime_url() -> &'static str {
    let url = database_url();
    let (scheme, rest) = url.split_once("://").expect("postgres URL");
    let host = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    Box::leak(format!("{scheme}://{RUNTIME_USER}:{RUNTIME_PASSWORD}@{host}").into_boxed_str())
}

/// The API as production composes it (default approval mode = human decision), dev auth, plus
/// `extra` settings.
async fn app_with(tenant: &'static str, extra: &[(&'static str, &'static str)]) -> Router {
    let _ = owner_pool().await; // migrations + runtime role
    let mut src: HashMap<&'static str, &'static str> = HashMap::from([
        ("SUTRA_ENV", "development"),
        ("DATABASE_URL", runtime_url()),
        ("SUTRA_SESSION_STORE", "postgres"),
        ("SUTRA_IDEMPOTENCY_STORE", "postgres"),
        ("SUTRA_AUDIT_SINK", "postgres"),
        ("SUTRA_DEV_TENANT_ID", tenant),
        ("SUTRA_SWEEPERS", "false"),
        ("SUTRA_INBOX_CONSUMER", "false"),
    ]);
    src.extend(extra.iter().copied());
    let cfg = Config::load(&src).expect("config");
    assert_eq!(
        cfg.hardening.workflow.approvals,
        ApprovalMode::AwaitDecision,
        "the default approval mode is a human decision"
    );
    router(AppContext::from_config(&cfg).expect("context"))
}

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

/// `(bank_account_number, bank_ifsc, pending_bank_account_number)` straight from the table.
async fn bank_row(pool: &sqlx::PgPool, sid: &str) -> (String, String, Option<String>) {
    sqlx::query_as(
        "SELECT bank_account_number, bank_ifsc, pending_bank_account_number \
         FROM resources.supplier WHERE id = $1::uuid",
    )
    .bind(sid)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// `(actor_id, details)` of the succeeded audit events `action` on `target_id`.
async fn audit_events(
    pool: &sqlx::PgPool,
    action: &str,
    target_id: &str,
) -> Vec<(Option<String>, Value)> {
    sqlx::query_as(
        "SELECT actor_id, details FROM platform.audit_log WHERE tenant_id = $1::uuid \
         AND action = $2 AND target_id = $3 AND outcome = 'succeeded' ORDER BY occurred_at",
    )
    .bind(T_BANK)
    .bind(action)
    .bind(target_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn events(pool: &sqlx::PgPool, event_type: &str, subject: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox WHERE event_type = $1 AND subject = $2",
    )
    .bind(event_type)
    .bind(subject)
    .fetch_one(pool)
    .await
    .unwrap()
}

// ============================================================================ DEF-J-004

#[tokio::test]
async fn supplier_bank_details_change_only_through_an_independent_approval() {
    // Setup (institution, active supplier) with the auto-approving setup app; the governed steps
    // run through the default (human) composition on the same database and tenant.
    let (setup_app, pool) = app(T_BANK).await;
    let admin = login(&setup_app, ADMIN).await;
    let setup =
        b8::campus_setup(&setup_app, &admin, &pool, T_BANK, "Bank Control University").await;
    let inst = setup.institution().as_uuid().to_string();
    let s = call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/suppliers"),
        json!({"legalName": "Acme Traders", "pan": "ABCDE1234F", "bankAccountName": "Acme Traders",
            "bankAccountNumber": OLD_ACCOUNT, "bankIfsc": "HDFC0001234"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let sid = id(&s);
    let s = call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/suppliers/{sid}/register"),
        json!({}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    let s = call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/suppliers/{sid}/submit"),
        json!({}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["status"], "active", "{s}");

    let app = app_with(T_BANK, &[]).await;
    let admin = login(&app, ADMIN).await;
    let checker = login(&app, PROCUREMENT_APPROVER).await;
    let finance = login(&app, FINANCE_APPROVER).await;
    let faculty = login(&app, FACULTY).await;
    let path = format!("{R}/suppliers/{sid}");
    let s = read(&app, &admin, &path).await;

    // ---- the attack: redirect payments by patching the profile → refused, nothing changed.
    let (st, _, b) = send(
        &app,
        write(
            &admin,
            "PATCH",
            &path,
            json!({"bankAccountNumber": NEW_ACCOUNT, "bankIfsc": "ICIC0000001"}),
            Some(ver(&s)),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "bank PATCH must be refused: {b}");
    assert!(b.to_string().contains("bank-change"), "{b}");
    assert_eq!(
        bank_row(&pool, &sid).await,
        (OLD_ACCOUNT.into(), "HDFC0001234".into(), None)
    );
    // Other profile fields still update; resending the unchanged IFSC is a no-op.
    let s = call(
        &app,
        &admin,
        "PATCH",
        &path,
        json!({"email": "accounts@acme.example.in", "bankIfsc": "HDFC0001234"}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["email"], "accounts@acme.example.in");
    // Any other writer is stopped by the database (trigger), even with table privileges.
    let direct =
        sqlx::query("UPDATE resources.supplier SET bank_account_number = $2 WHERE id = $1::uuid")
            .bind(&sid)
            .bind(NEW_ACCOUNT)
            .execute(&pool)
            .await;
    assert!(
        direct.is_err_and(|e| e.to_string().contains("bank-change request")),
        "direct UPDATE must be refused by the trigger"
    );

    // ---- the legitimate path: a request (reason required), pending, live details unchanged.
    let (st, _, b) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{path}/bank-change"),
            json!({"bankAccountNumber": NEW_ACCOUNT, "bankIfsc": "ICIC0000001", "reason": ""}),
            Some(ver(&s)),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "reason required: {b}");
    let s = call(
        &app,
        &admin,
        "POST",
        &format!("{path}/bank-change"),
        json!({"bankAccountNumber": NEW_ACCOUNT, "bankIfsc": "ICIC0000001",
            "reason": "Supplier letter dated 1 Dec: account moved to ICICI"}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["bankChangePending"], true, "{s}");
    assert_eq!(
        s["bankAccountNumber"], "XXXXXXXX7890",
        "live account unchanged"
    );
    assert_eq!(
        s["pendingBankAccountNumber"], "XXXXXXXX5544",
        "pending masked"
    );
    assert_eq!(s["pendingBankIfsc"], "ICIC0000001");
    assert_eq!(s["bankChangeRequestedBy"], "dev|admin@sutra.dev");
    assert_eq!(
        bank_row(&pool, &sid).await,
        (
            OLD_ACCOUNT.into(),
            "HDFC0001234".into(),
            Some(NEW_ACCOUNT.into())
        ),
        "payments keep using the approved account while the request is open"
    );
    assert_eq!(
        events(
            &pool,
            "sutraerp.resources.supplier.bank_change_requested",
            &sid
        )
        .await,
        1,
        "approvers are notified"
    );
    let request_id = s["bankChangeRequestId"].as_str().unwrap().to_owned();

    // One open request at a time; the PATCH route is still closed.
    let (st, _, b) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{path}/bank-change"),
            json!({"bankAccountNumber": SECOND_ACCOUNT, "bankIfsc": "SBIN0000123", "reason": "again"}),
            Some(ver(&s)),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");

    // ---- separation of duties: the maker and a user without an approve permission → 403.
    let decision = format!("{path}/bank-change/decision");
    let (st, _, b) = send(
        &app,
        write(
            &admin,
            "POST",
            &decision,
            json!({"decision": "approve"}),
            Some(ver(&s)),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "maker self-approval: {b}");
    let (st, _, b) = send(
        &app,
        write(
            &faculty,
            "POST",
            &decision,
            json!({"decision": "approve"}),
            Some(ver(&s)),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "no approve permission: {b}");
    assert_eq!(bank_row(&pool, &sid).await.0, OLD_ACCOUNT);

    // ---- an independent checker approves → the new account is live and everything recorded.
    let s = call(
        &app,
        &checker,
        "POST",
        &decision,
        json!({"decision": "approve", "reason": "Confirmed by call-back to the supplier"}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["bankChangePending"], false, "{s}");
    assert_eq!(s["bankAccountNumber"], "XXXXXXXX5544");
    assert_eq!(s["bankIfsc"], "ICIC0000001");
    assert_eq!(
        bank_row(&pool, &sid).await,
        (NEW_ACCOUNT.into(), "ICIC0000001".into(), None)
    );
    let hist: Vec<(Option<String>, String, Option<String>)> = sqlx::query_as(
        "SELECT from_status, to_status, actor FROM resources.status_history \
         WHERE aggregate_type = 'supplier_bank_change' AND aggregate_id = $1::uuid \
         ORDER BY created_at, id",
    )
    .bind(&request_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        hist,
        vec![
            (None, "pending".into(), Some("dev|admin@sutra.dev".into())),
            (
                Some("pending".into()),
                "approved".into(),
                Some("dev|procurement.approver@sutra.dev".into())
            ),
        ]
    );
    let decided = audit_events(&pool, "resources.supplier.bank_change.decide", &sid).await;
    assert_eq!(decided.len(), 1);
    assert_eq!(
        decided[0].0.as_deref(),
        Some("dev|procurement.approver@sutra.dev")
    );
    assert_eq!(decided[0].1["requestedBy"], "dev|admin@sutra.dev");
    assert_eq!(decided[0].1["oldAccount"], "XXXXXXXX7890 (HDFC0001234)");
    assert_eq!(decided[0].1["newAccount"], "XXXXXXXX5544 (ICIC0000001)");
    assert_eq!(
        audit_events(&pool, "resources.supplier.bank_change.request", &sid)
            .await
            .len(),
        1
    );
    assert_eq!(
        events(
            &pool,
            "sutraerp.resources.supplier.bank_change_approved",
            &sid
        )
        .await,
        1,
        "finance is warned"
    );

    // ---- a second request, rejected by a finance approver → the approved account stays. The
    // maker of the first request is not barred from deciding a later one by somebody else.
    let s = call(
        &app,
        &admin,
        "POST",
        &format!("{path}/bank-change"),
        json!({"bankAccountNumber": SECOND_ACCOUNT, "bankIfsc": "SBIN0000123",
            "reason": "E-mail from accounts@acme"}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    let (st, _, b) = send(
        &app,
        write(
            &finance,
            "POST",
            &decision,
            json!({"decision": "reject"}),
            Some(ver(&s)),
        ),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::UNPROCESSABLE_ENTITY,
        "reject needs a reason: {b}"
    );
    let s = call(
        &app,
        &finance,
        "POST",
        &decision,
        json!({"decision": "reject", "reason": "Not confirmed on call-back: likely phishing"}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["bankChangePending"], false);
    assert_eq!(
        bank_row(&pool, &sid).await,
        (NEW_ACCOUNT.into(), "ICIC0000001".into(), None)
    );
    assert_eq!(
        events(
            &pool,
            "sutraerp.resources.supplier.bank_change_rejected",
            &sid
        )
        .await,
        1
    );
    // No decision without an open request.
    let (st, _, _) = send(
        &app,
        write(
            &checker,
            "POST",
            &decision,
            json!({"decision": "approve"}),
            Some(ver(&s)),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);

    // ---- full account numbers never reach the audit log or the outbox.
    for acct in [NEW_ACCOUNT, SECOND_ACCOUNT] {
        let leaks: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM platform.audit_log WHERE tenant_id = $1::uuid \
                     AND details::text LIKE '%' || $2 || '%') \
                  + (SELECT count(*) FROM platform.outbox WHERE subject = $3 \
                     AND envelope::text LIKE '%' || $2 || '%')",
        )
        .bind(T_BANK)
        .bind(acct)
        .bind(&sid)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(leaks, 0, "{acct} leaked into audit / outbox");
    }
}

// ============================================================================ transport

async fn raw(app: &Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn assert_hardened(h: &axum::http::HeaderMap, what: &str) {
    assert_eq!(h["x-content-type-options"], "nosniff", "{what}");
    assert_eq!(h["x-frame-options"], "DENY", "{what}");
    let csp = h["content-security-policy"].to_str().unwrap();
    assert!(
        csp.starts_with("default-src 'none'") && csp.contains("frame-ancestors 'none'"),
        "{what}: {csp}"
    );
    assert_eq!(h["referrer-policy"], "no-referrer", "{what}");
    assert_eq!(
        h["strict-transport-security"], "max-age=31536000; includeSubDomains",
        "{what}"
    );
}

#[tokio::test]
async fn api_transport_hardening_end_to_end() {
    let app = app_with(
        T_HTTP,
        &[
            ("SUTRA_HSTS_MAX_AGE_SECS", "31536000"),
            ("SUTRA_ALLOWED_ORIGINS", "https://partner.example.edu"),
            ("SUTRA_MAX_BODY_BYTES", "4096"),
            ("SUTRA_RATE_LIMIT", "true"),
            ("SUTRA_RATE_LIMIT_CREDENTIALS_PER_MIN", "6"),
            ("SUTRA_RATE_LIMIT_CREDENTIALS_BURST", "2"),
            ("SUTRA_API_DOCS", "false"),
            ("SUTRA_HEALTH_DETAIL", "false"),
        ],
    )
    .await;

    // Headers on a normal answer, a 404 and a dependency-free probe.
    let (st, h, _) = raw(
        &app,
        Request::get("/health/live").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_hardened(&h, "/health/live");
    assert_eq!(h["cache-control"], "no-store");
    let (st, h, _) = raw(
        &app,
        Request::get("/api/v1/no-such-route")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_hardened(&h, "404");

    // The OpenAPI document is not served (production rule); the default development app serves it.
    let (st, _, _) = raw(
        &app,
        Request::get("/api/openapi.json")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "SUTRA_API_DOCS=false");
    let dev = app_with(T_HTTP, &[]).await;
    let (st, _, body) = raw(
        &dev,
        Request::get("/api/openapi.json")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(body.contains("\"openapi\""));

    // Readiness reports check names, never dependency error texts.
    let (_, _, body) = raw(
        &app,
        Request::get("/health/ready").body(Body::empty()).unwrap(),
    )
    .await;
    assert!(!body.contains("\"detail\""), "{body}");

    // CORS: only the configured origin, echoed (never `*`), credentials allowed.
    let (_, h, _) = raw(
        &app,
        Request::get("/health/live")
            .header("origin", "https://partner.example.edu")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        h["access-control-allow-origin"],
        "https://partner.example.edu"
    );
    let (_, h, _) = raw(
        &app,
        Request::get("/health/live")
            .header("origin", "https://evil.example")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(h.get("access-control-allow-origin").is_none());
    let (st, _, _) = raw(
        &app,
        Request::post("/api/v1/auth/logout")
            .header("origin", "https://evil.example")
            .header("sec-fetch-site", "cross-site")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "cross-site unsafe request still refused"
    );

    // Body limit: 413 above SUTRA_MAX_BODY_BYTES, with the hardening headers.
    let big = format!(
        "{{\"loginId\":\"{}\",\"password\":\"x\"}}",
        "a".repeat(8192)
    );
    let (st, h, _) = raw(
        &app,
        Request::post("/api/v1/auth/login")
            .header("content-type", "application/json")
            .header("content-length", big.len().to_string())
            .body(Body::from(big))
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE);
    assert_hardened(&h, "413");

    // Password guessing: two wrong passwords are answered, the third is throttled (429 +
    // Retry-After) — even with a valid pre-session CSRF token.
    let mut statuses = vec![];
    let mut last = None;
    for _ in 0..3 {
        let (s, h, body) = send(
            &app,
            Request::get("/api/v1/auth/csrf")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let cookie = h["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let token = body["csrfToken"].as_str().unwrap().to_owned();
        let (st, h, _) = raw(
            &app,
            Request::post("/api/v1/auth/login")
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .header("x-csrf-token", &token)
                .body(Body::from(
                    json!({"loginId": ADMIN, "password": "wrong-guess"}).to_string(),
                ))
                .unwrap(),
        )
        .await;
        statuses.push(st);
        last = Some(h);
    }
    assert_ne!(statuses[0], StatusCode::TOO_MANY_REQUESTS);
    assert_ne!(statuses[1], StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(statuses[2], StatusCode::TOO_MANY_REQUESTS, "{statuses:?}");
    let h = last.unwrap();
    assert!(h["retry-after"].to_str().unwrap().parse::<u64>().unwrap() >= 1);
    assert_hardened(&h, "429");
}
