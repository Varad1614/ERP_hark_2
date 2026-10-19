//! Part 1 Stage 3 (auth) — attack-style router tests of the assembled application.
//! No database: the pool points at a closed port, so any handler that reaches persistence
//! answers `503` after the 1 s acquire timeout — which is exactly how the route sweep below
//! detects a handler that touched data before checking authentication / a permission.
//!
//! * `privileged_role_needs_mfa_for_every_privileged_permission` — SEC-P1-AUTH-001
//! * `api_responses_carry_hardening_headers` — SEC-9D-006 / SEC-9D-012
//! * `every_route_requires_a_session_and_a_permission` — SEC-P1-AUTH-005 (route authz sweep)

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use sutra_api::app::{router, AppContext};
use sutra_api::config::{Config, DEV_TENANT_ID};
use sutra_contracts::platform::auth::{NewSession, Principal};
use sutra_kernel::scope::ExecutionScope;
use tower::ServiceExt;

const INSTITUTIONS: &str = "/api/v1/institution/institutions";
const SOME_ID: &str = "0192f5a0-7c3e-7000-8000-0000000000aa";

fn context(extra: &[(&'static str, &'static str)]) -> AppContext {
    let mut src: HashMap<&'static str, &'static str> = HashMap::from([
        ("SUTRA_ENV", "test"),
        ("SUTRA_DEV_AUTH_PASSWORD", sutra_api::app::SPEC_DEV_PASSWORD),
        ("DATABASE_URL", "postgres://sutra:sutra@127.0.0.1:1/sutra"),
        ("SUTRA_DB_ACQUIRE_TIMEOUT_SECS", "1"),
    ]);
    src.extend(extra.iter().copied());
    AppContext::from_config(&Config::load(&src).unwrap()).unwrap()
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn first_cookie(h: &axum::http::HeaderMap) -> String {
    h[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

struct Login {
    cookie: String,
    csrf: String,
}

async fn login(app: &Router, login_id: &str) -> (Login, Value) {
    let (s, h, body) = send(
        app,
        Request::get("/api/v1/auth/csrf")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let pre = first_cookie(&h);
    let token = body["csrfToken"].as_str().unwrap().to_owned();
    let (s, h, body) = send(
        app,
        Request::post("/api/v1/auth/login")
            .header("content-type", "application/json")
            .header("cookie", &pre)
            .header("x-csrf-token", &token)
            .body(Body::from(
                json!({"loginId": login_id, "password": sutra_api::app::SPEC_DEV_PASSWORD})
                    .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    (
        Login {
            cookie: first_cookie(&h),
            csrf: body["csrfToken"].as_str().unwrap().to_owned(),
        },
        body,
    )
}

fn write(method: &str, path: &str, who: &Login, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("cookie", &who.cookie)
        .header("x-csrf-token", &who.csrf)
        .header("if-match", "\"1\"")
        .header("idempotency-key", uuid::Uuid::now_v7().to_string())
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn read(path: &str, who: &Login) -> Request<Body> {
    Request::get(path)
        .header("cookie", &who.cookie)
        .body(Body::empty())
        .unwrap()
}

// ---------------------------------------------------------------------------------------------
// SEC-P1-AUTH-001: privileged roles need MFA for every permission they hold through that role
// ---------------------------------------------------------------------------------------------

/// Attack: a stolen administrator password (no second factor) used against a production-like
/// deployment. Before the fix only the 25 "step-up" permissions needed MFA, so a password-only
/// administrator session could still rename institutions, manage students, charges, payroll
/// setup, procurement… With privileged-role MFA enforced every privileged permission answers
/// `403 mfa-step-up-required` until the session verified a second factor; the personal
/// baseline (own notifications, profile) keeps working so the user can step up.
#[tokio::test]
async fn privileged_role_needs_mfa_for_every_privileged_permission() {
    let rename = json!({"name": "Renamed"});
    let path = format!("{INSTITUTIONS}/{SOME_ID}");

    // Control: without enforcement (dev default) the password-only admin reaches the
    // repository (503: no database) — the policy let it through.
    let open = router(context(&[]));
    let (admin, _) = login(&open, "admin@sutra.dev").await;
    let (s, _, b) = send(&open, write("PATCH", &path, &admin, rename.clone())).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{b}");

    // Enforced (production / Keycloak default).
    let app = router(context(&[("SUTRA_PRIVILEGED_MFA", "true")]));
    let (admin, body) = login(&app, "admin@sutra.dev").await;
    assert_eq!(body["mfaLevel"], "none");
    assert_eq!(
        body["mfaEnrollmentRequired"], true,
        "told to enrol a factor"
    );
    let (s, _, b) = send(&app, write("PATCH", &path, &admin, rename.clone())).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
    assert_eq!(b["errorCode"], "mfa-step-up-required");
    let (s, _, b) = send(&app, read(&path, &admin)).await;
    assert!(
        (s == StatusCode::FORBIDDEN && b["errorCode"] == "mfa-step-up-required")
            || s == StatusCode::SERVICE_UNAVAILABLE,
        "institution reads are privileged unless every role holds them: {s} {b}"
    );
    // Personal baseline (own notifications) is not blocked by the policy (503 = repository).
    let (s, _, b) = send(&app, read("/api/v1/notifications", &admin)).await;
    assert_ne!(b["errorCode"], "mfa-step-up-required", "{s} {b}");
    // A non-privileged role is unaffected.
    let (faculty, body) = login(&app, "meera.shah@sutra.dev").await;
    assert_eq!(body["mfaEnrollmentRequired"], false);
    let (s, _, b) = send(&app, read("/api/v1/notifications", &faculty)).await;
    assert_ne!(b["errorCode"], "mfa-step-up-required", "{s} {b}");

    // Enrol + confirm TOTP: the session is rotated to MFA and the privileged permission passes.
    use sutra_platform::auth::mfa::{base32_decode, totp_code, totp_step};
    let (s, _, enrol) = send(
        &app,
        write("POST", "/api/v1/auth/mfa/totp/enrolment", &admin, json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{enrol}");
    let secret = base32_decode(enrol["secret"].as_str().unwrap()).unwrap();
    let code = totp_code(&secret, totp_step(chrono::Utc::now().timestamp()));
    let (s, h, body) = send(
        &app,
        write(
            "POST",
            "/api/v1/auth/mfa/totp/confirm",
            &admin,
            json!({"code": code}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["mfaLevel"], "mfa");
    let elevated = Login {
        cookie: first_cookie(&h),
        csrf: body["csrfToken"].as_str().unwrap().to_owned(),
    };
    let (s, _, b) = send(&app, write("PATCH", &path, &elevated, rename)).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "policy passed: {b}");
}

// ---------------------------------------------------------------------------------------------
// SEC-9D-006 / SEC-9D-012: hardening headers on API responses (the ingress routes /api here)
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn api_responses_carry_hardening_headers() {
    let app = router(context(&[]));
    for (path, expected) in [
        ("/health/live", StatusCode::OK),
        ("/api/v1/auth/session", StatusCode::UNAUTHORIZED),
        ("/api/v1/nope", StatusCode::NOT_FOUND),
    ] {
        let (s, h, _) = send(&app, Request::get(path).body(Body::empty()).unwrap()).await;
        assert_eq!(s, expected, "{path}");
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{path}");
        assert_eq!(h[header::X_FRAME_OPTIONS], "DENY", "{path}");
        assert_eq!(h[header::CACHE_CONTROL], "no-store", "{path}");
        assert_eq!(h[header::REFERRER_POLICY], "no-referrer", "{path}");
        assert_eq!(h["cross-origin-resource-policy"], "same-origin", "{path}");
        assert!(
            h.get(header::STRICT_TRANSPORT_SECURITY).is_none(),
            "dev: no HSTS"
        );
    }
    let tls = router(context(&[("SUTRA_HSTS", "true")]));
    let (_, h, _) = send(
        &tls,
        Request::get("/health/live").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(
        h[header::STRICT_TRANSPORT_SECURITY],
        "max-age=31536000; includeSubDomains"
    );
}

// ---------------------------------------------------------------------------------------------
// SEC-P1-AUTH-005: every route requires a session AND a permission (except an explicit list)
// ---------------------------------------------------------------------------------------------

/// Routes that answer without a session, by design (each justified in
/// docs/readiness/security/auth.md): health probes, the sign-in bootstrap (pre-session CSRF
/// protected), the OIDC redirect endpoints, the signed back-channel logout and payment webhook,
/// idempotent logout, the static navigation catalog (the same JSON ships in the web bundle) and
/// the public careers page (ERP_EXTENSIONS 5A).
const PUBLIC: [(&str, &str); 17] = [
    ("GET", "/health/live"),
    ("GET", "/health/ready"),
    ("GET", "/api/v1/auth/csrf"),
    ("POST", "/api/v1/auth/login"),
    ("POST", "/api/v1/auth/mfa/verify"),
    ("POST", "/api/v1/auth/mfa/webauthn/challenge"),
    ("POST", "/api/v1/auth/mfa/webauthn/verify"),
    ("GET", "/api/v1/auth/oidc/start"),
    ("GET", "/api/v1/auth/oidc/callback"),
    ("POST", "/api/v1/auth/oidc/backchannel-logout"),
    ("POST", "/api/v1/auth/logout"),
    ("POST", "/api/v1/finance/gateway/webhook"),
    // ERP_EXTENSIONS 5A: the public careers page (read-only list + spam-safe apply form).
    ("GET", "/api/v1/people/public/careers/{id}"),
    (
        "POST",
        "/api/v1/people/public/careers/{id}/requisitions/{requisition_id}/apply",
    ),
    ("GET", "/api/v1/platform/modules"),
    ("GET", "/api/v1/platform/workspaces"),
    ("GET", "/api/v1/platform/workspaces/{key}"),
];

/// Authenticated-only routes that act on the caller's *own* session / second factors (or return
/// only data addressed to the caller's own roles) and
/// therefore need a session but no role permission.
const SESSION_ONLY: [(&str, &str); 17] = [
    ("GET", "/api/v1/auth/session"),
    ("GET", "/api/v1/auth/mfa"),
    ("DELETE", "/api/v1/auth/mfa/totp"),
    ("POST", "/api/v1/auth/mfa/totp/enrolment"),
    ("POST", "/api/v1/auth/mfa/totp/confirm"),
    ("POST", "/api/v1/auth/mfa/step-up"),
    ("POST", "/api/v1/auth/mfa/webauthn/registration"),
    ("POST", "/api/v1/auth/mfa/webauthn/registration/complete"),
    ("POST", "/api/v1/auth/mfa/webauthn/assertion"),
    ("POST", "/api/v1/auth/mfa/webauthn/step-up"),
    ("DELETE", "/api/v1/auth/mfa/webauthn/credentials/{id}"),
    ("GET", "/api/v1/auth/sessions"),
    ("POST", "/api/v1/auth/sessions/revoke-others"),
    ("DELETE", "/api/v1/auth/sessions/{id}"),
    // Dashboards published to the caller's own roles (the query filters by `ctx.roles`; a
    // role-less session gets an empty list, never another role's board).
    ("GET", "/api/v1/services/me/dashboards"),
    // Part 2 · S7-entitle: the caller's own tenant's active modules / licence state and the
    // public module catalogue (no tenant data). Every signed-in user needs them to see which
    // modules are on and why a request was refused; changes stay behind
    // `platform.entitlement.manage` / `.approve`.
    ("GET", "/api/v1/platform/entitlement"),
    ("GET", "/api/v1/platform/entitlement/catalogue"),
];

fn resolve<'a>(doc: &'a Value, schema: &'a Value) -> &'a Value {
    match schema.get("$ref").and_then(Value::as_str) {
        Some(r) => {
            let name = r.rsplit('/').next().unwrap_or_default();
            &doc["components"]["schemas"][name]
        }
        None => schema,
    }
}

/// A minimal instance of `schema` (required members only) so the JSON body extractor accepts
/// the request and the handler's own checks run.
fn example(doc: &Value, schema: &Value, depth: usize) -> Value {
    let s = resolve(doc, schema);
    if depth > 8 {
        return Value::Null;
    }
    if let Some(v) = s.get("example") {
        return v.clone();
    }
    if let Some(e) = s
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    {
        return e.clone();
    }
    if let Some(c) = s.get("const") {
        return c.clone();
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(first) = s.get(key).and_then(Value::as_array).and_then(|a| {
            a.iter()
                .find(|x| x.get("type").and_then(Value::as_str) != Some("null"))
        }) {
            return example(doc, first, depth + 1);
        }
    }
    if let Some(all) = s.get("allOf").and_then(Value::as_array) {
        let mut out = serde_json::Map::new();
        for part in all {
            if let Value::Object(m) = example(doc, part, depth + 1) {
                out.extend(m);
            }
        }
        return Value::Object(out);
    }
    let ty = match s.get("type") {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null")
            .unwrap_or("null")
            .to_owned(),
        _ if s.get("properties").is_some() => "object".to_owned(),
        _ => "string".to_owned(),
    };
    match ty.as_str() {
        "object" => {
            let mut out = serde_json::Map::new();
            let required: Vec<&str> = s
                .get("required")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if let Some(props) = s.get("properties").and_then(Value::as_object) {
                for (k, v) in props {
                    if required.contains(&k.as_str()) {
                        out.insert(k.clone(), example(doc, v, depth + 1));
                    }
                }
            }
            Value::Object(out)
        }
        "array" => {
            let min = s.get("minItems").and_then(Value::as_u64).unwrap_or(0);
            if min > 0 {
                let item = s.get("items").cloned().unwrap_or(Value::Null);
                Value::Array(vec![example(doc, &item, depth + 1)])
            } else {
                json!([])
            }
        }
        "integer" => json!(s.get("minimum").and_then(Value::as_i64).unwrap_or(1).max(1)),
        "number" => json!(1),
        "boolean" => json!(false),
        "null" => Value::Null,
        _ => match s.get("format").and_then(Value::as_str) {
            Some("uuid") => json!(SOME_ID),
            Some("date") => json!("2026-01-15"),
            Some("date-time") => json!("2026-01-15T10:00:00Z"),
            Some("email") => json!("probe@sutra.test"),
            Some("uri") => json!("https://example.edu/x"),
            _ => json!("probe"),
        },
    }
}

fn param_value(doc: &Value, p: &Value) -> String {
    match example(doc, &p["schema"], 0) {
        Value::String(s) => s,
        Value::Null => "probe".to_owned(),
        other => other.to_string(),
    }
}

struct Probe {
    method: String,
    template: String,
    uri: String,
    body: Option<String>,
}

fn probes(doc: &Value) -> Vec<Probe> {
    let mut out = vec![];
    for (template, item) in doc["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            if !["get", "post", "put", "patch", "delete"].contains(&method.as_str()) {
                continue;
            }
            let params: Vec<&Value> = op
                .get("parameters")
                .and_then(Value::as_array)
                .map(|a| a.iter().collect())
                .unwrap_or_default();
            let mut uri = template.clone();
            let mut query = vec![];
            for p in &params {
                let name = p["name"].as_str().unwrap_or_default();
                match p["in"].as_str() {
                    Some("path") => {
                        uri = uri.replace(&format!("{{{name}}}"), &param_value(doc, p));
                    }
                    Some("query") if p["required"].as_bool() == Some(true) => {
                        query.push(format!("{name}={}", param_value(doc, p)));
                    }
                    _ => {}
                }
            }
            // Any `{x}` left without a declared parameter: a UUID.
            while let (Some(a), Some(b)) = (uri.find('{'), uri.find('}')) {
                if b < a {
                    break;
                }
                uri.replace_range(a..=b, SOME_ID);
            }
            if !query.is_empty() {
                uri = format!("{uri}?{}", query.join("&"));
            }
            let body = op
                .get("requestBody")
                .and_then(|rb| rb["content"].get("application/json"))
                .map(|c| example(doc, &c["schema"], 0).to_string());
            out.push(Probe {
                method: method.to_uppercase(),
                template: template.clone(),
                uri,
                body,
            });
        }
    }
    out
}

fn probe_request(p: &Probe, session: Option<(&str, &str)>) -> Request<Body> {
    let mut b = Request::builder()
        .method(Method::from_bytes(p.method.as_bytes()).unwrap())
        .uri(&p.uri)
        .header("if-match", "\"1\"")
        .header("idempotency-key", uuid::Uuid::now_v7().to_string());
    if let Some((cookie, csrf)) = session {
        b = b.header("cookie", cookie).header("x-csrf-token", csrf);
    }
    match &p.body {
        Some(body) => b
            .header("content-type", "application/json")
            .body(Body::from(body.clone()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

/// Status / error code of every probe for which `include` holds, run concurrently (each
/// DB-reaching request costs the 1 s acquire timeout). Excluded probes are not sent.
async fn run(
    app: &Router,
    probes: &[Probe],
    session: Option<(String, String)>,
    include: impl Fn(&Probe) -> bool,
) -> Vec<(StatusCode, String)> {
    let sem = Arc::new(tokio::sync::Semaphore::new(64));
    let mut set = tokio::task::JoinSet::new();
    for (i, p) in probes.iter().enumerate() {
        if !include(p) {
            continue;
        }
        let req = probe_request(p, session.as_ref().map(|(c, t)| (c.as_str(), t.as_str())));
        let app = app.clone();
        let sem = sem.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.unwrap();
            let (s, _, b) = send(&app, req).await;
            (i, s, b["errorCode"].as_str().unwrap_or_default().to_owned())
        });
    }
    let mut out = vec![(StatusCode::OK, String::new()); probes.len()];
    while let Some(r) = set.join_next().await {
        let (i, s, code) = r.unwrap();
        out[i] = (s, code);
    }
    out
}

/// Attack: call every API operation (from the served OpenAPI document) (a) without a session
/// and (b) with a live session that holds **no role** (a valid but unprivileged identity).
///
/// (a) must answer `401` for everything outside [`PUBLIC`]; (b) must answer `403 forbidden`
/// for everything outside [`PUBLIC`] / [`SESSION_ONLY`] — or be rejected by request
/// extraction (`400` / `415` / `422` / `428`) before any handler logic runs. A `2xx`, `404`,
/// `409` or `503` (the handler reached the repository) means a route that lacks an
/// authentication or permission check before touching data.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_route_requires_a_session_and_a_permission() {
    let ctx = context(&[]);
    let sessions = ctx.platform.sessions.clone();
    let cookie_name = ctx.platform.cookie.name();
    let issued = sessions
        .create(NewSession::password(Principal {
            subject: "probe|roleless".into(),
            display_name: "Roleless probe".into(),
            login_id: "probe@sutra.test".into(),
            roles: vec![],
            scope: ExecutionScope::parse(Some(DEV_TENANT_ID), None).unwrap(),
        }))
        .await
        .unwrap();
    let session = (
        format!("{cookie_name}={}", issued.token),
        issued.session.csrf_token.clone(),
    );
    let app = router(ctx);
    let (s, _, doc) = send(
        &app,
        Request::get("/api/openapi.json")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let probes = probes(&doc);
    assert!(probes.len() > 1000, "only {} operations", probes.len());
    let is_in = |list: &[(&str, &str)], p: &Probe| {
        list.iter().any(|(m, t)| *m == p.method && *t == p.template)
    };

    let anonymous = run(&app, &probes, None, |p| !is_in(&PUBLIC, p)).await;
    // The roleless run never sends the public / own-session routes: `POST /auth/logout`,
    // `DELETE /auth/sessions/{id}` and `revoke-others` would end the probe session mid-sweep
    // and every later probe would (wrongly) see `401`.
    let roleless = run(&app, &probes, Some(session), |p| {
        !is_in(&PUBLIC, p) && !is_in(&SESSION_ONLY, p)
    })
    .await;

    let extraction = [
        StatusCode::BAD_REQUEST,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        StatusCode::UNPROCESSABLE_ENTITY,
        StatusCode::PRECONDITION_REQUIRED,
    ];
    let mut failures: BTreeMap<String, String> = BTreeMap::new();
    let (mut checked, mut inconclusive) = (0usize, 0usize);
    for (i, p) in probes.iter().enumerate() {
        let key = format!("{} {}", p.method, p.template);
        if is_in(&PUBLIC, p) {
            continue;
        }
        let (s, code) = &anonymous[i];
        if *s != StatusCode::UNAUTHORIZED {
            failures.insert(key.clone(), format!("anonymous → {s} {code}"));
            continue;
        }
        if is_in(&SESSION_ONLY, p) {
            continue;
        }
        let (s, code) = &roleless[i];
        if *s == StatusCode::FORBIDDEN && (code == "forbidden" || code == "scope-violation") {
            checked += 1;
        } else if extraction.contains(s) {
            inconclusive += 1;
        } else {
            failures.insert(key, format!("roleless session → {s} {code}"));
        }
    }
    let report: Vec<String> = failures.iter().map(|(k, v)| format!("{k}: {v}")).collect();
    assert!(
        failures.is_empty(),
        "{} route(s) without an auth / permission check before data access \
         (checked {checked}, inconclusive {inconclusive}):\n{}",
        failures.len(),
        report.join("\n")
    );
    println!(
        "route authz sweep: {} operations, {checked} refused a roleless session with 403, \
         {inconclusive} rejected at request extraction",
        probes.len()
    );
    // The sweep must actually exercise the permission layer for most routes.
    assert!(
        checked * 2 > probes.len(),
        "only {checked} of {} operations reached the permission check",
        probes.len()
    );
}
