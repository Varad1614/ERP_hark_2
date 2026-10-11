//! Router tests of the auth boundary (in-memory stores, in-process test IdP; no database).

use super::*;
use crate::auth::dev::{DevAuthenticator, PrivilegedRoleMfaPolicy};
use crate::auth::keycloak::test_idp::{TestIdp, CLIENT_ID, TENANT};
use crate::auth::keycloak::KeycloakAuthenticator;
use crate::auth::mfa::{
    base32_decode, totp_code, totp_step, DeferredWebauthn, InMemoryMfaFactorStore, SecretBox,
};
use crate::auth::preauth::InMemoryPreAuthStore;
use crate::auth::replay::InMemoryReplayGuard;
use crate::auth::session::InMemorySessionStore;
use crate::auth::webauthn::soft::SoftAuthenticator;
use crate::auth::webauthn::{DevWebauthn, InMemoryWebauthnStore, WebauthnSettings};
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use std::sync::atomic::{AtomicI64, Ordering};
use sutra_contracts::platform::auth::SessionPolicy;
use sutra_kernel::audit::TracingAuditSink;
use sutra_kernel::scope::{ExecutionScope, TenantId};
use tower::ServiceExt;

const LOGIN_CSRF_COOKIE: &str = "__Host-sutra_login_csrf";
const WEBAUTHN_ORIGIN: &str = "http://localhost:5173";

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
    let pre_auth: Arc<dyn PreAuthStore> = Arc::new(InMemoryPreAuthStore::default());
    let webauthn: Arc<dyn WebauthnPort> = if oidc.is_some() {
        Arc::new(DeferredWebauthn)
    } else {
        Arc::new(
            DevWebauthn::new(
                &WebauthnSettings {
                    rp_id: "localhost".into(),
                    rp_name: "SutraERP".into(),
                    origins: vec![WEBAUTHN_ORIGIN.into()],
                },
                Arc::new(InMemoryWebauthnStore::default()),
                pre_auth.clone(),
            )
            .unwrap(),
        )
    };
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
        pre_auth,
        mfa_service: if oidc.is_some() { None } else { Some(mfa) },
        oidc,
        webauthn,
        pre_session,
        replay: Arc::new(InMemoryReplayGuard::default()),
        sign_in_hook: None,
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

// ---------------------------------------------------------------------------------------------
// Batch 1D: dev-mode WebAuthn over HTTP
// ---------------------------------------------------------------------------------------------

fn post(path: &str, body: serde_json::Value, cookie: &str, csrf: &str) -> Request<Body> {
    request("POST", path, body, &[cookie], Some(csrf))
}

/// Registers a passkey for the signed-in session; returns (authenticator, rotated cookie, csrf).
async fn register_passkey(
    app: &axum::Router,
    cookie: &str,
    csrf: &str,
) -> (SoftAuthenticator, String, String) {
    let res = app
        .clone()
        .oneshot(post(
            "/api/v1/auth/mfa/webauthn/registration",
            serde_json::json!({}),
            cookie,
            csrf,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let c = json(res).await;
    let mut auth = SoftAuthenticator::new("localhost", WEBAUTHN_ORIGIN);
    let credential = auth.create(&c["options"]);
    let res = app
        .clone()
        .oneshot(post(
            "/api/v1/auth/mfa/webauthn/registration/complete",
            serde_json::json!({"ceremonyId": c["ceremonyId"], "credential": credential, "label": "Laptop"}),
            cookie,
            csrf,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let rotated = session_cookie(&res);
    assert_ne!(rotated, cookie, "registration rotates the session");
    let body = json(res).await;
    assert_eq!(body["mfaLevel"], "mfa");
    (
        auth,
        rotated,
        body["csrfToken"].as_str().unwrap().to_owned(),
    )
}

#[tokio::test]
async fn passkey_registration_then_sign_in_challenge_with_replay_and_browser_binding() {
    let h = harness(true, None);
    let (cookie, csrf, body) = login(&h.app, "admin@sutra.dev").await;
    assert_eq!(body["mfaEnrollmentRequired"], true);
    let (mut auth, cookie, csrf) = register_passkey(&h.app, &cookie, &csrf).await;
    let status = json(
        h.app
            .clone()
            .oneshot(get("/api/v1/auth/mfa", &cookie))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status["managedBy"], "sutra");
    assert_eq!(status["passkeys"][0]["label"], "Laptop");
    assert_eq!(status["totpEnrolled"], false);
    assert_eq!(status["mfaRequired"], true);
    let _ = (cookie, csrf);

    // Sign in again: the passkey is offered at the challenge.
    let pre = login_csrf(&h.app).await;
    let res = h
        .app
        .clone()
        .oneshot(login_req("admin@sutra.dev", "sutra", &pre, None))
        .await
        .unwrap();
    let body = json(res).await;
    assert_eq!(body["outcome"], "mfaRequired");
    assert_eq!(body["mfaMethods"], serde_json::json!(["webauthn"]));
    let challenge = body["challengeId"].as_str().unwrap().to_owned();
    let challenge_req = |id: &str, pre: &LoginCsrf| {
        request(
            "POST",
            "/api/v1/auth/mfa/webauthn/challenge",
            serde_json::json!({"challengeId": id}),
            &[&pre.cookie],
            Some(&pre.token),
        )
    };
    // Another browser (different login CSRF token) cannot use the challenge — and burns it.
    let other = login_csrf(&h.app).await;
    let res = h
        .app
        .clone()
        .oneshot(challenge_req(&challenge, &other))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let res = h
        .app
        .clone()
        .oneshot(challenge_req(&challenge, &pre))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "challenge is single use"
    );

    // Fresh login → passkey challenge → assertion → MFA session.
    let res = h
        .app
        .clone()
        .oneshot(login_req("admin@sutra.dev", "sutra", &pre, None))
        .await
        .unwrap();
    let challenge = json(res).await["challengeId"].as_str().unwrap().to_owned();
    let res = h
        .app
        .clone()
        .oneshot(challenge_req(&challenge, &pre))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let c = json(res).await;
    let new_id = c["challengeId"].as_str().unwrap().to_owned();
    assert_ne!(new_id, challenge);
    assert!(c["options"]["publicKey"]["allowCredentials"].is_array());
    let assertion = auth.get(&c["options"], true);
    let verify = |id: &str, cred: &serde_json::Value| {
        request(
            "POST",
            "/api/v1/auth/mfa/webauthn/verify",
            serde_json::json!({"challengeId": id, "credential": cred}),
            &[&pre.cookie],
            Some(&pre.token),
        )
    };
    let res = h
        .app
        .clone()
        .oneshot(verify(&new_id, &assertion))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cookie = session_cookie(&res);
    let body = json(res).await;
    assert_eq!(body["outcome"], "authenticated");
    assert_eq!(body["mfaLevel"], "mfa");
    let me = json(
        h.app
            .clone()
            .oneshot(get("/api/v1/auth/session", &cookie))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(me["amr"], serde_json::json!(["pwd", "hwk", "mfa"]));
    // Replaying the verified response is refused (challenge consumed).
    let res = h
        .app
        .clone()
        .oneshot(verify(&new_id, &assertion))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    // Verify without having started the passkey option is refused.
    let res = h
        .app
        .clone()
        .oneshot(login_req("admin@sutra.dev", "sutra", &pre, None))
        .await
        .unwrap();
    let plain = json(res).await["challengeId"].as_str().unwrap().to_owned();
    let res = h
        .app
        .clone()
        .oneshot(verify(&plain, &assertion))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    // The pre-session endpoints require the login CSRF token.
    let res = h
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/mfa/webauthn/challenge",
            serde_json::json!({"challengeId": "x"}),
            &[],
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn passkey_step_up_removal_and_totp_disable_need_an_mfa_session() {
    let h = harness(false, None);
    let (cookie, csrf, _) = login(&h.app, "admin@sutra.dev").await;
    // No factor yet: registering does not need step-up.
    let (mut auth, _, _) = register_passkey(&h.app, &cookie, &csrf).await;
    // A new single-factor session (MFA not enforced at login).
    let (cookie, csrf, body) = login(&h.app, "admin@sutra.dev").await;
    assert_eq!(body["mfaLevel"], "none");
    let res = h
        .app
        .clone()
        .oneshot(post(
            "/api/v1/auth/mfa/webauthn/registration",
            serde_json::json!({}),
            &cookie,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(json(res).await["errorCode"], "mfa-step-up-required");
    let status = json(
        h.app
            .clone()
            .oneshot(get("/api/v1/auth/mfa", &cookie))
            .await
            .unwrap(),
    )
    .await;
    let passkey_id = status["passkeys"][0]["id"].as_str().unwrap().to_owned();
    let res = h
        .app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/auth/mfa/webauthn/credentials/{passkey_id}"),
            serde_json::json!({}),
            &[&cookie],
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "removal needs step-up");
    // Step up with the passkey.
    let res = h
        .app
        .clone()
        .oneshot(post(
            "/api/v1/auth/mfa/webauthn/assertion",
            serde_json::json!({}),
            &cookie,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let c = json(res).await;
    let ok = auth.get(&c["options"], true);
    let res = h
        .app
        .clone()
        .oneshot(post(
            "/api/v1/auth/mfa/webauthn/step-up",
            serde_json::json!({"ceremonyId": c["ceremonyId"], "credential": ok}),
            &cookie,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let elevated = session_cookie(&res);
    let csrf = json(res).await["csrfToken"].as_str().unwrap().to_owned();
    // A later assertion whose sign counter did not advance (cloned authenticator) is refused.
    let c = json(
        h.app
            .clone()
            .oneshot(post(
                "/api/v1/auth/mfa/webauthn/assertion",
                serde_json::json!({}),
                &elevated,
                &csrf,
            ))
            .await
            .unwrap(),
    )
    .await;
    let stale = auth.get(&c["options"], false);
    let res = h
        .app
        .clone()
        .oneshot(post(
            "/api/v1/auth/mfa/webauthn/step-up",
            serde_json::json!({"ceremonyId": c["ceremonyId"], "credential": stale}),
            &elevated,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "sign counter did not advance"
    );
    // TOTP disable without a TOTP factor → 404; removing the passkey now works (MFA not required).
    let res = h
        .app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/auth/mfa/totp",
            serde_json::json!({}),
            &[&elevated],
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = h
        .app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/auth/mfa/webauthn/credentials/{passkey_id}"),
            serde_json::json!({}),
            &[&elevated],
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn last_factor_of_an_account_that_requires_mfa_cannot_be_removed() {
    let h = harness(true, None);
    let (cookie, csrf, _) = login(&h.app, "admin@sutra.dev").await;
    let (secret, cookie, csrf) = enrol(&h, &cookie, &csrf).await;
    let _ = secret;
    let res = h
        .app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/auth/mfa/totp",
            serde_json::json!({}),
            &[&cookie],
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    // With a passkey as well, the authenticator app can go.
    let (_, cookie, csrf) = register_passkey(&h.app, &cookie, &csrf).await;
    let res = h
        .app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/auth/mfa/totp",
            serde_json::json!({}),
            &[&cookie],
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn sessions_list_revoke_one_and_revoke_others() {
    let app = app(false);
    let (a, csrf_a, _) = login(&app, "meera.shah@sutra.dev").await;
    let (b, _, _) = login(&app, "meera.shah@sutra.dev").await;
    let (c, _, _) = login(&app, "meera.shah@sutra.dev").await;
    let (other_user, _, _) = login(&app, "aarav.desai@sutra.dev").await;
    let list = json(
        app.clone()
            .oneshot(get("/api/v1/auth/sessions", &a))
            .await
            .unwrap(),
    )
    .await;
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 3, "only this subject's sessions");
    assert_eq!(items.iter().filter(|s| s["current"] == true).count(), 1);
    let current = items.iter().find(|s| s["current"] == true).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let victim = items.iter().find(|s| s["current"] == false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let del = |id: &str| {
        request(
            "DELETE",
            &format!("/api/v1/auth/sessions/{id}"),
            serde_json::json!({}),
            &[&a],
            Some(&csrf_a),
        )
    };
    assert_eq!(
        app.clone().oneshot(del(&current)).await.unwrap().status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        app.clone().oneshot(del(&victim)).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        app.clone().oneshot(del(&victim)).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    let res = app
        .clone()
        .oneshot(post(
            "/api/v1/auth/sessions/revoke-others",
            serde_json::json!({}),
            &a,
            &csrf_a,
        ))
        .await
        .unwrap();
    assert_eq!(json(res).await["revoked"], 1);
    for dead in [&b, &c] {
        assert_eq!(
            app.clone()
                .oneshot(get("/api/v1/auth/session", dead))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    for alive in [&a, &other_user] {
        assert_eq!(
            app.clone()
                .oneshot(get("/api/v1/auth/session", alive))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
    // CSRF is enforced on revoke-others.
    let res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/sessions/revoke-others",
            serde_json::json!({}),
            &[&a],
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------------------------
// Batch 1D: OIDC back-channel logout
// ---------------------------------------------------------------------------------------------

const LOGOUT_EVENT: &str = "http://schemas.openid.net/event/backchannel-logout";

fn logout_claims(o: &Oidc, sid: &str) -> serde_json::Value {
    let now = chrono::Utc::now().timestamp();
    serde_json::json!({
        "iss": o.idp.issuer,
        "aud": CLIENT_ID,
        "iat": now,
        "exp": now + 120,
        "jti": uuid::Uuid::now_v7().to_string(),
        "sid": sid,
        "sub": "f3b9c1de-0000-4000-8000-00000000a001",
        "events": { LOGOUT_EVENT: {} },
    })
}

fn backchannel(token: &str) -> Request<Body> {
    Request::post("/api/v1/auth/oidc/backchannel-logout")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(format!("logout_token={token}")))
        .unwrap()
}

/// OIDC sign-in returning the session cookie (IdP session `kc-session-1`).
async fn oidc_session(o: &Oidc) -> String {
    let s = start(o, "").await;
    *o.idp.state.id_token.lock().unwrap() = Some(TestIdp::sign(&o.idp.claims(&s.nonce)));
    let res = callback(o, &s.state, &s.binding_cookie).await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    session_cookie(&res)
}

async fn alive(o: &Oidc, cookie: &str) -> bool {
    o.h.app
        .clone()
        .oneshot(get("/api/v1/auth/session", cookie))
        .await
        .unwrap()
        .status()
        == StatusCode::OK
}

#[tokio::test]
async fn backchannel_logout_validates_the_token_and_revokes_the_idp_session() {
    use crate::auth::keycloak::test_idp::KEY_B;
    let o = oidc().await;
    let cookie = oidc_session(&o).await;
    let send = |token: String| {
        let app = o.h.app.clone();
        async move { app.oneshot(backchannel(&token)).await.unwrap() }
    };

    // Invalid tokens are refused with 400 and revoke nothing.
    let mut bad = vec![];
    // bad signature: signed with key B but claiming key A's kid
    bad.push(TestIdp::sign_with(
        &logout_claims(&o, "kc-session-1"),
        KEY_B,
        Some("test-key-a"),
    ));
    let mut c = logout_claims(&o, "kc-session-1");
    c.as_object_mut().unwrap().remove("events");
    bad.push(TestIdp::sign(&c)); // missing events
    let mut c = logout_claims(&o, "kc-session-1");
    c["events"] = serde_json::json!({ "urn:other": {} });
    bad.push(TestIdp::sign(&c)); // wrong event
    let mut c = logout_claims(&o, "kc-session-1");
    c["nonce"] = serde_json::json!("n");
    bad.push(TestIdp::sign(&c)); // nonce present (looks like an ID token)
    let mut c = logout_claims(&o, "kc-session-1");
    c.as_object_mut().unwrap().remove("sid");
    c.as_object_mut().unwrap().remove("sub");
    bad.push(TestIdp::sign(&c)); // neither sid nor sub
    let mut c = logout_claims(&o, "kc-session-1");
    c["aud"] = serde_json::json!("another-client");
    bad.push(TestIdp::sign(&c)); // wrong audience
    let mut c = logout_claims(&o, "kc-session-1");
    c["iss"] = serde_json::json!("https://evil.example/realms/x");
    bad.push(TestIdp::sign(&c)); // wrong issuer
    let mut c = logout_claims(&o, "kc-session-1");
    c.as_object_mut().unwrap().remove("jti");
    bad.push(TestIdp::sign(&c)); // no jti
    let mut c = logout_claims(&o, "kc-session-1");
    let old = chrono::Utc::now().timestamp() - 3600;
    c["iat"] = serde_json::json!(old);
    c["exp"] = serde_json::json!(old + 60);
    bad.push(TestIdp::sign(&c)); // expired
    bad.push("not-a-jwt".into());
    for (i, token) in bad.into_iter().enumerate() {
        let res = send(token).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "case {i}");
        assert!(alive(&o, &cookie).await, "case {i} must not revoke");
    }
    // Missing token.
    let res =
        o.h.app
            .clone()
            .oneshot(
                Request::post("/api/v1/auth/oidc/backchannel-logout")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("foo=bar"))
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // A token for another IdP session leaves this one alone.
    let res = send(TestIdp::sign(&logout_claims(&o, "kc-session-other"))).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(alive(&o, &cookie).await);

    // The valid token revokes the session bound to sid; replaying it is refused.
    let token = TestIdp::sign(&logout_claims(&o, "kc-session-1"));
    let res = send(token.clone()).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    assert!(res.headers().get(header::SET_COOKIE).is_none());
    assert!(
        !alive(&o, &cookie).await,
        "session revoked by back-channel logout"
    );
    let res = send(token).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "jti replay");

    // sub-only token: every federated session of the subject ends.
    let c1 = oidc_session(&o).await;
    let mut c = logout_claims(&o, "x");
    c.as_object_mut().unwrap().remove("sid");
    assert_eq!(send(TestIdp::sign(&c)).await.status(), StatusCode::OK);
    assert!(!alive(&o, &c1).await);
}

#[tokio::test]
async fn backchannel_logout_and_passkeys_are_mode_specific() {
    // Dev mode: no IdP → 501.
    let res = app(false).oneshot(backchannel("x")).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
    // Keycloak mode: passkeys are Keycloak's → 501 problem.
    let o = oidc().await;
    let cookie = oidc_session(&o).await;
    let me = json(
        o.h.app
            .clone()
            .oneshot(get("/api/v1/auth/session", &cookie))
            .await
            .unwrap(),
    )
    .await;
    let csrf = me["csrfToken"].as_str().unwrap().to_owned();
    for path in [
        "/api/v1/auth/mfa/webauthn/registration",
        "/api/v1/auth/mfa/webauthn/assertion",
    ] {
        let res =
            o.h.app
                .clone()
                .oneshot(post(path, serde_json::json!({}), &cookie, &csrf))
                .await
                .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED, "{path}");
        assert_eq!(
            res.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
    }
    let status = json(
        o.h.app
            .clone()
            .oneshot(get("/api/v1/auth/mfa", &cookie))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status["managedBy"], "keycloak");
    assert_eq!(status["webauthnAvailable"], false);
}
