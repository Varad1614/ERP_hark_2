//! Router tests of the auth boundary (in-memory stores, in-process test IdP; no database).

use super::*;
use crate::auth::dev::{DevAuthenticator, PrivilegedRoleMfaPolicy};
use crate::auth::keycloak::test_idp::{TestIdp, CLIENT_ID, TENANT};
use crate::auth::keycloak::KeycloakAuthenticator;
use crate::auth::mfa::{
    base32_decode, totp_code, totp_step, DeferredWebauthn, InMemoryMfaFactorStore, SecretBox,
};
use crate::auth::preauth::InMemoryPreAuthStore;
use crate::auth::session::InMemorySessionStore;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use std::sync::atomic::{AtomicI64, Ordering};
use sutra_contracts::platform::auth::SessionPolicy;
use sutra_kernel::audit::TracingAuditSink;
use sutra_kernel::scope::{ExecutionScope, TenantId};
use tower::ServiceExt;

const LOGIN_CSRF_COOKIE: &str = "__Host-sutra_login_csrf";

struct Harness {
    app: axum::Router,
    /// Unix seconds seen by the TOTP verifier.
    clock: Arc<AtomicI64>,
}

fn harness(enforce_mfa: bool, oidc: Option<Arc<dyn OidcIdentityProvider>>) -> Harness {
    let sessions: Arc<dyn SessionStore> = Arc::new(InMemorySessionStore::new(
        SessionPolicy::new(1800, 3600).unwrap(),
    ));
    let cookie = CookieSettings {
        secure: true,
        max_age_secs: 3600,
    };
    let pre_session = PreSessionCsrf::new([1u8; 32], true, 1800);
    let auth = crate::auth::session_auth(sessions.clone(), &cookie, pre_session.clone(), true);
    let clock = Arc::new(AtomicI64::new(1_900_000_000));
    let c = clock.clone();
    let mfa = MfaService::with_clock(
        Arc::new(InMemoryMfaFactorStore::default()),
        SecretBox::new(&[2u8; 32]).unwrap(),
        "SutraERP".into(),
        Arc::new(move || chrono::DateTime::from_timestamp(c.load(Ordering::SeqCst), 0).unwrap()),
    );
    let deps = PlatformDeps {
        authenticator: Arc::new(DevAuthenticator::new(
            "sutra".into(),
            ExecutionScope::tenant(TenantId::new()),
        )),
        sessions,
        mfa: Arc::new(PrivilegedRoleMfaPolicy {
            enforce: enforce_mfa,
        }),
        audit: Arc::new(TracingAuditSink),
        cookie,
        modules: vec![],
        pre_auth: Arc::new(InMemoryPreAuthStore::default()),
        mfa_service: if oidc.is_some() { None } else { Some(mfa) },
        oidc,
        webauthn: Arc::new(DeferredWebauthn),
        pre_session,
    };
    let app = axum::Router::from(router(deps)).layer(axum::middleware::from_fn_with_state(
        auth,
        sutra_web::auth::authenticate,
    ));
    Harness { app, clock }
}

fn app(enforce_mfa: bool) -> axum::Router {
    harness(enforce_mfa, None).app
}

/// A pre-session CSRF pair: (cookie header fragment, token).
#[derive(Clone)]
struct LoginCsrf {
    cookie: String,
    token: String,
}

async fn login_csrf(app: &axum::Router) -> LoginCsrf {
    let res = app
        .clone()
        .oneshot(
            Request::get("/api/v1/auth/csrf")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    let set = res.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(set.starts_with(LOGIN_CSRF_COOKIE));
    assert!(set.contains("HttpOnly") && set.contains("SameSite=Strict") && set.contains("Secure"));
    let cookie = set.split(';').next().unwrap().to_owned();
    let token = json(res).await["csrfToken"].as_str().unwrap().to_owned();
    assert!(
        cookie.ends_with(&token),
        "double submit: cookie and body carry the same value"
    );
    LoginCsrf { cookie, token }
}

fn request(
    method: &str,
    path: &str,
    body: serde_json::Value,
    cookies: &[&str],
    csrf: Option<&str>,
) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    let jar: Vec<&str> = cookies.iter().copied().filter(|c| !c.is_empty()).collect();
    if !jar.is_empty() {
        b = b.header("cookie", jar.join("; "));
    }
    if let Some(t) = csrf {
        b = b.header(CSRF_HEADER, t);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

fn login_req(id: &str, pw: &str, csrf: &LoginCsrf, session: Option<&str>) -> Request<Body> {
    request(
        "POST",
        "/api/v1/auth/login",
        serde_json::json!({"loginId": id, "password": pw}),
        &[&csrf.cookie, session.unwrap_or("")],
        Some(&csrf.token),
    )
}

fn get(path: &str, cookie: &str) -> Request<Body> {
    Request::get(path)
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap()
}

async fn json(res: Response) -> serde_json::Value {
    serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

fn session_cookie(res: &Response) -> String {
    res.headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap())
        .find(|v| v.starts_with("__Host-sutra_session="))
        .expect("session cookie")
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

/// Signs in through the login CSRF bootstrap. Returns (session cookie, session CSRF, body).
async fn login(app: &axum::Router, id: &str) -> (String, String, serde_json::Value) {
    let csrf = login_csrf(app).await;
    let res = app
        .clone()
        .oneshot(login_req(id, "sutra", &csrf, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cookie = session_cookie(&res);
    let body = json(res).await;
    let token = body["csrfToken"].as_str().unwrap().to_owned();
    (cookie, token, body)
}

// ---------------------------------------------------------------------------------------------
// Sessions + login CSRF
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn login_session_logout_flow() {
    let app = app(false);
    let csrf = login_csrf(&app).await;
    let res = app
        .clone()
        .oneshot(login_req("aarav.desai@sutra.dev", "sutra", &csrf, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let set = res.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(set.contains("HttpOnly") && set.contains("Secure") && set.contains("SameSite=Strict"));
    let cookie = session_cookie(&res);
    let body = json(res).await;
    assert_eq!(body["outcome"], "authenticated");
    assert_eq!(body["principal"]["roles"][0], "student");
    assert_eq!(body["mfaLevel"], "none");
    assert_eq!(body["mfaEnrollmentRequired"], false);
    assert!(!body.to_string().contains("password"));
    let token = body["csrfToken"].as_str().unwrap().to_owned();
    assert_eq!(token.len(), 64);
    assert_ne!(
        token, csrf.token,
        "the session gets its own synchronizer token"
    );

    let me = app
        .clone()
        .oneshot(get("/api/v1/auth/session", &cookie))
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::OK);
    assert_eq!(me.headers()[header::CACHE_CONTROL], "no-store");
    let me = json(me).await;
    assert_eq!(me["principal"]["displayName"], "Aarav Desai");
    assert_eq!(me["csrfToken"], token.as_str());
    assert_eq!(me["mfaLevel"], "none");
    assert_eq!(me["amr"], serde_json::json!(["pwd"]));

    // Forged logout (no session CSRF token) must not end the session.
    let forced = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/logout",
            serde_json::json!({}),
            &[&cookie],
            None,
        ))
        .await
        .unwrap();
    assert_eq!(forced.status(), StatusCode::FORBIDDEN);
    assert_eq!(json(forced).await["errorCode"], "csrf-token-missing");
    // The pre-session token is not a session token either.
    let wrong = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/logout",
            serde_json::json!({}),
            &[&cookie, &csrf.cookie],
            Some(&csrf.token),
        ))
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::FORBIDDEN);

    let out = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/logout",
            serde_json::json!({}),
            &[&cookie],
            Some(&token),
        ))
        .await
        .unwrap();
    assert_eq!(out.status(), StatusCode::NO_CONTENT);
    let me = app
        .oneshot(get("/api/v1/auth/session", &cookie))
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_csrf_missing_invalid_and_ok() {
    let app = app(false);
    let body = serde_json::json!({"loginId": "admin@sutra.dev", "password": "sutra"});
    // missing
    let res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            body.clone(),
            &[],
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(res.headers().get(header::SET_COOKIE).is_none());
    assert_eq!(json(res).await["errorCode"], "csrf-token-missing");
    // header only (attacker cannot set the __Host- cookie)
    let csrf = login_csrf(&app).await;
    let res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            body.clone(),
            &[],
            Some(&csrf.token),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(json(res).await["errorCode"], "csrf-token-invalid");
    // cookie of one bootstrap, header of another
    let other = login_csrf(&app).await;
    let res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            body.clone(),
            &[&csrf.cookie],
            Some(&other.token),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(json(res).await["errorCode"], "csrf-token-invalid");
    // a self-made, unsigned pair
    let fake = format!(
        "{}.{}.{}",
        "a".repeat(64),
        Utc::now().timestamp() + 600,
        "b".repeat(64)
    );
    let res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            body.clone(),
            &[&format!("{LOGIN_CSRF_COOKIE}={fake}")],
            Some(&fake),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    // ok
    let res = app
        .clone()
        .oneshot(login_req("admin@sutra.dev", "sutra", &csrf, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    // MFA verify is protected the same way.
    let res = app
        .oneshot(request(
            "POST",
            "/api/v1/auth/mfa/verify",
            serde_json::json!({"challengeId": "x", "code": "123456"}),
            &[],
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(json(res).await["errorCode"], "csrf-token-missing");
}

#[tokio::test]
async fn logout_without_live_session_is_idempotent() {
    let res = app(false)
        .oneshot(request(
            "POST",
            "/api/v1/auth/logout",
            serde_json::json!({}),
            &["__Host-sutra_session=dead"],
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn bad_credentials_are_generic_problem() {
    let app = app(false);
    let csrf = login_csrf(&app).await;
    let res = app
        .oneshot(login_req("admin@sutra.dev", "nope", &csrf, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        res.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    assert_eq!(json(res).await["errorCode"], "unauthenticated");
}

#[tokio::test]
async fn empty_fields_are_validation_errors() {
    let app = app(false);
    let csrf = login_csrf(&app).await;
    let res = app.oneshot(login_req("", "", &csrf, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json(res).await["validationErrors"].as_array().map(Vec::len),
        Some(2)
    );
}

#[tokio::test]
async fn login_rotates_presented_session() {
    let app = app(false);
    let (first, first_csrf, _) = login(&app, "meera.shah@sutra.dev").await;
    let csrf = login_csrf(&app).await;
    let res = app
        .clone()
        .oneshot(login_req(
            "meera.shah@sutra.dev",
            "sutra",
            &csrf,
            Some(&first),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let second = session_cookie(&res);
    assert_ne!(first, second);
    assert_ne!(json(res).await["csrfToken"].as_str().unwrap(), first_csrf);
    let old = app
        .oneshot(get("/api/v1/auth/session", &first))
        .await
        .unwrap();
    assert_eq!(old.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn csrf_token_of_another_session_is_rejected() {
    let app = app(false);
    let (cookie_a, _, _) = login(&app, "meera.shah@sutra.dev").await;
    let (_, csrf_b, _) = login(&app, "aarav.desai@sutra.dev").await;
    let res = app
        .oneshot(request(
            "POST",
            "/api/v1/auth/logout",
            serde_json::json!({}),
            &[&cookie_a],
            Some(&csrf_b),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(json(res).await["errorCode"], "csrf-token-invalid");
}

// ---------------------------------------------------------------------------------------------
// TOTP MFA (dev mode)
// ---------------------------------------------------------------------------------------------

/// Enrols TOTP for the signed-in session; returns (secret bytes, rotated cookie, csrf).
async fn enrol(h: &Harness, cookie: &str, csrf: &str) -> (Vec<u8>, String, String) {
    let res = h
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/mfa/totp/enrolment",
            serde_json::json!({}),
            &[cookie],
            Some(csrf),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    let body = json(res).await;
    let uri = body["otpauthUri"].as_str().unwrap();
    assert!(uri.starts_with("otpauth://totp/SutraERP:admin@sutra.dev?secret="));
    assert_eq!(body["digits"], 6);
    let secret = base32_decode(body["secret"].as_str().unwrap()).unwrap();
    // wrong first code
    let res = h
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/mfa/totp/confirm",
            serde_json::json!({"code": "000000"}),
            &[cookie],
            Some(csrf),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let code = totp_code(&secret, totp_step(h.clock.load(Ordering::SeqCst)));
    let res = h
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/mfa/totp/confirm",
            serde_json::json!({"code": code}),
            &[cookie],
            Some(csrf),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let rotated = session_cookie(&res);
    assert_ne!(rotated, cookie, "privilege change rotates the session id");
    let body = json(res).await;
    assert_eq!(body["mfaLevel"], "mfa");
    assert_eq!(body["mfaEnrollmentRequired"], false);
    (
        secret,
        rotated,
        body["csrfToken"].as_str().unwrap().to_owned(),
    )
}

#[tokio::test]
async fn totp_enrol_then_login_challenge_verify_and_replay() {
    let h = harness(true, None);
    // 1. Admin without a factor: single-factor session flagged for enrolment.
    let (cookie, csrf, body) = login(&h.app, "admin@sutra.dev").await;
    assert_eq!(body["mfaEnrollmentRequired"], true);
    assert_eq!(body["mfaLevel"], "none");
    let me = json(
        h.app
            .clone()
            .oneshot(get("/api/v1/auth/session", &cookie))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(me["mfaEnrollmentRequired"], true);
    let (secret, rotated, _) = enrol(&h, &cookie, &csrf).await;
    assert_eq!(
        h.app
            .clone()
            .oneshot(get("/api/v1/auth/session", &cookie))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED,
        "pre-enrolment session id is revoked"
    );
    let me = json(
        h.app
            .clone()
            .oneshot(get("/api/v1/auth/session", &rotated))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(me["mfaLevel"], "mfa");
    assert!(me["amr"].as_array().unwrap().iter().any(|a| a == "otp"));

    // 2. Next sign-in: password → challenge (no session yet).
    let lc = login_csrf(&h.app).await;
    let res = h
        .app
        .clone()
        .oneshot(login_req("admin@sutra.dev", "sutra", &lc, None))
        .await
        .unwrap();
    assert!(
        res.headers().get(header::SET_COOKIE).is_none(),
        "no session before MFA"
    );
    let body = json(res).await;
    assert_eq!(body["outcome"], "mfaRequired");
    assert_eq!(body["mfaMethods"], serde_json::json!(["totp"]));
    assert!(body.get("csrfToken").is_none());
    let challenge = body["challengeId"].as_str().unwrap().to_owned();
    let verify = |challenge: &str, code: &str, lc: &LoginCsrf| {
        request(
            "POST",
            "/api/v1/auth/mfa/verify",
            serde_json::json!({"challengeId": challenge, "code": code}),
            &[&lc.cookie],
            Some(&lc.token),
        )
    };
    // The enrolment code's step is spent: replay within the same step fails (and burns the
    // single-use challenge).
    let now = h.clock.load(Ordering::SeqCst);
    let same_step = totp_code(&secret, totp_step(now));
    let res = h
        .app
        .clone()
        .oneshot(verify(&challenge, &same_step, &lc))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    // A burned challenge cannot be retried, even with a good code.
    h.clock.store(now + 30, Ordering::SeqCst);
    let next = totp_code(&secret, totp_step(now + 30));
    let res = h
        .app
        .clone()
        .oneshot(verify(&challenge, &next, &lc))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Fresh challenge, presented from another browser (other pre-session token) → refused.
    let res = h
        .app
        .clone()
        .oneshot(login_req("admin@sutra.dev", "sutra", &lc, None))
        .await
        .unwrap();
    let challenge = json(res).await["challengeId"].as_str().unwrap().to_owned();
    let other_browser = login_csrf(&h.app).await;
    let res = h
        .app
        .clone()
        .oneshot(verify(&challenge, &next, &other_browser))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Fresh challenge, right browser, current code → MFA session.
    let res = h
        .app
        .clone()
        .oneshot(login_req("admin@sutra.dev", "sutra", &lc, None))
        .await
        .unwrap();
    let challenge = json(res).await["challengeId"].as_str().unwrap().to_owned();
    let res = h
        .app
        .clone()
        .oneshot(verify(&challenge, &next, &lc))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers().get(header::SET_COOKIE).is_some());
    let body = json(res).await;
    assert_eq!(body["mfaLevel"], "mfa");
    assert!(body["csrfToken"].is_string());

    // Same code again on yet another challenge: replay within the same step.
    let res = h
        .app
        .clone()
        .oneshot(login_req("admin@sutra.dev", "sutra", &lc, None))
        .await
        .unwrap();
    let challenge = json(res).await["challengeId"].as_str().unwrap().to_owned();
    let res = h
        .app
        .clone()
        .oneshot(verify(&challenge, &next, &lc))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn step_up_rotates_an_aal1_session() {
    let h = harness(false, None);
    let (cookie, csrf, _) = login(&h.app, "admin@sutra.dev").await;
    let (secret, _, _) = enrol(&h, &cookie, &csrf).await;
    // MFA not enforced at login → single-factor session even with a factor.
    let (cookie, csrf, body) = login(&h.app, "admin@sutra.dev").await;
    assert_eq!(body["mfaLevel"], "none");
    let step_up = |code: &str, cookie: &str, csrf: &str| {
        request(
            "POST",
            "/api/v1/auth/mfa/step-up",
            serde_json::json!({"code": code}),
            &[cookie],
            Some(csrf),
        )
    };
    let res = h
        .app
        .clone()
        .oneshot(step_up("12", &cookie, &csrf))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let now = h.clock.load(Ordering::SeqCst);
    let replay = totp_code(&secret, totp_step(now));
    let res = h
        .app
        .clone()
        .oneshot(step_up(&replay, &cookie, &csrf))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "enrolment step already used"
    );
    h.clock.store(now + 30, Ordering::SeqCst);
    let code = totp_code(&secret, totp_step(now + 30));
    // Re-enrolment over an active factor needs step-up first.
    let res = h
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/mfa/totp/enrolment",
            serde_json::json!({}),
            &[&cookie],
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(json(res).await["errorCode"], "mfa-step-up-required");
    let res = h
        .app
        .clone()
        .oneshot(step_up(&code, &cookie, &csrf))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let elevated = session_cookie(&res);
    let body = json(res).await;
    assert_eq!(body["mfaLevel"], "mfa");
    let me = json(
        h.app
            .clone()
            .oneshot(get("/api/v1/auth/session", &elevated))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(me["mfaLevel"], "mfa");
    assert_eq!(
        h.app
            .clone()
            .oneshot(get("/api/v1/auth/session", &cookie))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    // Step-up without a session is 401.
    let res = h
        .app
        .oneshot(request(
            "POST",
            "/api/v1/auth/mfa/step-up",
            serde_json::json!({"code": "123456"}),
            &[],
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn webauthn_is_deferred_with_501_problem() {
    let app = app(false);
    let (cookie, csrf, _) = login(&app, "admin@sutra.dev").await;
    for path in [
        "/api/v1/auth/mfa/webauthn/registration",
        "/api/v1/auth/mfa/webauthn/assertion",
    ] {
        let res = app
            .clone()
            .oneshot(request(
                "POST",
                path,
                serde_json::json!({}),
                &[&cookie],
                Some(&csrf),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED, "{path}");
        assert_eq!(
            res.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
        assert_eq!(json(res).await["errorCode"], "not-implemented");
    }
}

// ---------------------------------------------------------------------------------------------
// OIDC (Keycloak mode) against the in-process test IdP
// ---------------------------------------------------------------------------------------------

struct Oidc {
    h: Harness,
    idp: TestIdp,
}

async fn oidc() -> Oidc {
    let idp = TestIdp::start().await;
    let adapter: Arc<dyn OidcIdentityProvider> =
        Arc::new(KeycloakAuthenticator::new(idp.settings()).unwrap());
    Oidc {
        h: harness(true, Some(adapter)),
        idp,
    }
}

struct Started {
    state: String,
    nonce: String,
    binding_cookie: String,
}

async fn start(o: &Oidc, query: &str) -> Started {
    let res =
        o.h.app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/auth/oidc/start{query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let loc = res.headers()[header::LOCATION].to_str().unwrap().to_owned();
    assert!(loc.starts_with(&format!("{}/protocol/openid-connect/auth?", o.idp.issuer)));
    let set = res.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        set.starts_with("__Host-sutra_oidc=")
            && set.contains("SameSite=Lax")
            && set.contains("HttpOnly")
    );
    let q: std::collections::HashMap<String, String> = reqwest::Url::parse(&loc)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["client_id"], CLIENT_ID);
    assert_eq!(q["state"].len(), 64);
    Started {
        state: q["state"].clone(),
        nonce: q["nonce"].clone(),
        binding_cookie: set.split(';').next().unwrap().to_owned(),
    }
}

async fn callback(o: &Oidc, state: &str, cookie: &str) -> Response {
    let mut b = Request::get(format!("/api/v1/auth/oidc/callback?code=abc&state={state}"));
    if !cookie.is_empty() {
        b = b.header("cookie", cookie);
    }
    o.h.app
        .clone()
        .oneshot(b.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

fn location(res: &Response) -> String {
    res.headers()[header::LOCATION].to_str().unwrap().to_owned()
}

#[tokio::test]
async fn oidc_start_requires_keycloak_mode() {
    let res = app(false)
        .oneshot(
            Request::get("/api/v1/auth/oidc/start")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn oidc_login_maps_roles_scope_and_mfa_then_rp_logout() {
    let o = oidc().await;
    let s = start(&o, "?returnTo=/workspaces").await;
    *o.idp.state.id_token.lock().unwrap() = Some(TestIdp::sign(&o.idp.claims(&s.nonce)));
    let res = callback(&o, &s.state, &s.binding_cookie).await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&res), "/workspaces");
    let cookie = session_cookie(&res);
    // PKCE verifier sent on the back channel matches the challenge sent to the browser.
    let form = o.idp.state.last_form.lock().unwrap().clone();
    assert_eq!(form["code"], "abc");
    assert_eq!(form["code_verifier"].len(), 43);

    let me = json(
        o.h.app
            .clone()
            .oneshot(get("/api/v1/auth/session", &cookie))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        me["principal"]["roles"],
        serde_json::json!(["administrator"])
    );
    assert_eq!(me["principal"]["scope"]["tenantId"], TENANT);
    assert_eq!(me["principal"]["loginId"], "admin@sutra.dev");
    assert_eq!(me["mfaLevel"], "mfa", "Keycloak asserted otp in amr");
    assert_eq!(
        me["mfaEnrollmentRequired"], false,
        "MFA is delegated to Keycloak"
    );

    // Replaying the same state is refused.
    let res = callback(&o, &s.state, &s.binding_cookie).await;
    assert_eq!(location(&res), "/sign-in?authError=state-invalid");

    // Password login and dev TOTP endpoints are not offered in Keycloak mode.
    let csrf = me["csrfToken"].as_str().unwrap().to_owned();
    let res =
        o.h.app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/auth/mfa/totp/enrolment",
                serde_json::json!({}),
                &[&cookie],
                Some(&csrf),
            ))
            .await
            .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);

    // RP-initiated logout.
    let res =
        o.h.app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/auth/logout",
                serde_json::json!({}),
                &[&cookie],
                Some(&csrf),
            ))
            .await
            .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let url = json(res).await["endSessionUrl"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(url.starts_with(&format!(
        "{}/protocol/openid-connect/logout?client_id={CLIENT_ID}",
        o.idp.issuer
    )));
    assert_eq!(
        o.h.app
            .clone()
            .oneshot(get("/api/v1/auth/session", &cookie))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn oidc_callback_rejects_state_mismatch_and_foreign_browser() {
    let o = oidc().await;
    let s = start(&o, "").await;
    *o.idp.state.id_token.lock().unwrap() = Some(TestIdp::sign(&o.idp.claims(&s.nonce)));
    let res = callback(&o, &"f".repeat(64), &s.binding_cookie).await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&res), "/sign-in?authError=state-invalid");
    assert!(res
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .all(|c| !c.to_str().unwrap().starts_with("__Host-sutra_session=")));
    let res = callback(&o, "", &s.binding_cookie).await;
    assert_eq!(location(&res), "/sign-in?authError=state-invalid");
    // Right state, but the callback lands in a browser that did not start the flow.
    let res = callback(&o, &s.state, "").await;
    assert_eq!(location(&res), "/sign-in?authError=browser-mismatch");
    // …and that consumed the state.
    let res = callback(&o, &s.state, &s.binding_cookie).await;
    assert_eq!(location(&res), "/sign-in?authError=state-invalid");
}

type ClaimMutator = Box<dyn Fn(&mut serde_json::Value)>;

#[tokio::test]
async fn oidc_callback_rejects_bad_nonce_expired_and_wrong_audience() {
    let o = oidc().await;
    let now = Utc::now().timestamp();
    let cases: Vec<(&str, ClaimMutator)> = vec![
        (
            "bad nonce",
            Box::new(|c| c["nonce"] = "not-the-nonce".into()),
        ),
        (
            "expired",
            Box::new(move |c| {
                c["exp"] = (now - 300).into();
                c["iat"] = (now - 900).into();
            }),
        ),
        (
            "wrong audience",
            Box::new(|c| {
                c["aud"] = "another-client".into();
                c["azp"] = "another-client".into();
            }),
        ),
    ];
    for (name, mutate) in cases {
        let s = start(&o, "").await;
        let mut claims = o.idp.claims(&s.nonce);
        mutate(&mut claims);
        *o.idp.state.id_token.lock().unwrap() = Some(TestIdp::sign(&claims));
        let res = callback(&o, &s.state, &s.binding_cookie).await;
        assert_eq!(location(&res), "/sign-in?authError=token-invalid", "{name}");
        assert!(
            res.headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .all(|c| !c.to_str().unwrap().starts_with("__Host-sutra_session=")),
            "{name}: no session"
        );
    }
}

#[tokio::test]
async fn oidc_role_mapping_and_unmapped_identities() {
    let o = oidc().await;
    // Faculty without MFA claims → single-factor faculty session.
    let s = start(&o, "?returnTo=//evil.example").await;
    let mut c = o.idp.claims(&s.nonce);
    c["realm_access"] = serde_json::json!({"roles": ["faculty", "default-roles-sutra"]});
    c["amr"] = serde_json::json!(["pwd"]);
    c["acr"] = "1".into();
    *o.idp.state.id_token.lock().unwrap() = Some(TestIdp::sign(&c));
    let res = callback(&o, &s.state, &s.binding_cookie).await;
    assert_eq!(
        location(&res),
        "/",
        "protocol-relative returnTo is not an open redirect"
    );
    let cookie = session_cookie(&res);
    let me = json(
        o.h.app
            .clone()
            .oneshot(get("/api/v1/auth/session", &cookie))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(me["principal"]["roles"], serde_json::json!(["faculty"]));
    assert_eq!(me["mfaLevel"], "none");
    // No SutraERP role → access denied, no session.
    let s = start(&o, "").await;
    let mut c = o.idp.claims(&s.nonce);
    c["realm_access"] = serde_json::json!({"roles": ["offline_access"]});
    *o.idp.state.id_token.lock().unwrap() = Some(TestIdp::sign(&c));
    let res = callback(&o, &s.state, &s.binding_cookie).await;
    assert_eq!(location(&res), "/sign-in?authError=access-denied");
    // No tenant claim → access denied (fail closed, no default tenant).
    let s = start(&o, "").await;
    let mut c = o.idp.claims(&s.nonce);
    c.as_object_mut().unwrap().remove("tenant_id");
    *o.idp.state.id_token.lock().unwrap() = Some(TestIdp::sign(&c));
    let res = callback(&o, &s.state, &s.binding_cookie).await;
    assert_eq!(location(&res), "/sign-in?authError=access-denied");
    // IdP-side error (user cancelled).
    let s = start(&o, "").await;
    let res =
        o.h.app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/auth/oidc/callback?error=access_denied&state={}",
                    s.state
                ))
                .header("cookie", &s.binding_cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(location(&res), "/sign-in?authError=access-denied");
}
