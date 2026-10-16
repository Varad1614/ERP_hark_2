//! Part 2 · Stage 4.3 (S4-idp) — identity provisioning against a **live Keycloak 26** (the compose
//! dev realm `deploy/keycloak/realm-sutra-dev.json`, imported by the CI `rust-db` job) and real
//! PostgreSQL. Nothing is mocked: the API is composed with `SUTRA_IDP_PROVISIONING=keycloak`
//! (Keycloak Admin REST adapter, least-privilege `sutra-idp-admin` service account) and the
//! sign-in runs through SutraERP's own OIDC endpoints (`SUTRA_AUTH_MODE=keycloak`) and Keycloak's
//! real login pages.
//!
//! ```sh
//! DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra SUTRA_TEST_KEYCLOAK_URL=http://localhost:8180 \
//!   cargo test -p sutra-api --features db-tests,keycloak-tests --test live_keycloak_idp -- --test-threads 1
//! ```
//!
//! * `provisioned_student_signs_in_and_suspension_locks_them_out` — the admin client passes the
//!   least-privilege start-up check; a provisioned student gets a Keycloak account in the
//!   `/sutra-tenant-<t>/institution-<i>` group with the `student` realm role; after activation
//!   (password set, `UPDATE_PASSWORD` completed on Keycloak's own page) a real OIDC sign-in through
//!   `/api/v1/auth/oidc/start` + `/callback` yields a SutraERP session whose tenant and institution
//!   come **from the group attributes** (risk 1); an ERP-role → realm-role mapping and a granted
//!   ERP role are applied through `role-mappings/realm/available` by a client **without**
//!   `view-realm` (risk 2) and are effective at the next sign-in; ERP suspension disables the
//!   account, ends the Keycloak sessions and the SutraERP session, and Keycloak refuses the next
//!   sign-in; restoration lets the student in again.
//! * `duplicate_identities_are_prevented_at_keycloak` — a pre-existing Keycloak account with the
//!   person's e-mail is adopted (never duplicated); an account in another tenant's group is never
//!   adopted (`conflict`); Keycloak itself refuses a second account with a taken username.
//! * `sync_retries_while_keycloak_is_unreachable` — with Keycloak cut off (TCP proxy dropping every
//!   connection) the sync fails with back-off; once it is reachable again the retry converges to
//!   exactly one account.
#![cfg(all(feature = "db-tests", feature = "keycloak-tests"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use support::*;
use sutra_api::app::{router, AppContext};
use sutra_api::config::Config;
use sutra_contracts::platform::identity::{NewIdpUser, ScopeGroup};
use sutra_persistence::jobs::run_once;
use sutra_platform::identity::sync::{IdpEventConsumer, IdpSyncWorker};
use sutra_platform::identity::IdentityRuntime;
use tower::ServiceExt;
use uuid::Uuid;

const T_LOGIN: &str = "0192f5a0-7c3e-7000-8000-0000000e1d01";
const T_DUP: &str = "0192f5a0-7c3e-7000-8000-0000000e1d02";
const T_RETRY: &str = "0192f5a0-7c3e-7000-8000-0000000e1d03";
const T_FOREIGN: &str = "0192f5a0-7c3e-7000-8000-0000000e1d04";

const ID: &str = "/api/v1/platform/identities";
const S: &str = "/api/v1/student";
const REALM: &str = "sutra";
/// Secret of the dev realm's `sutra-idp-admin` client (compose / CI only).
const IDP_ADMIN_SECRET: &str = "dev-only-idp-admin-secret";

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn kc_url() -> &'static str {
    leak(
        std::env::var("SUTRA_TEST_KEYCLOAK_URL")
            .unwrap_or_else(|_| "http://localhost:8180".into())
            .trim_end_matches('/')
            .to_owned(),
    )
}

fn runtime_db_url() -> &'static str {
    let url = database_url();
    let (scheme, rest) = url.split_once("://").unwrap();
    let host = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    leak(format!(
        "{scheme}://{RUNTIME_USER}:{RUNTIME_PASSWORD}@{host}"
    ))
}

/// Dev-auth app (administrators sign in with the dev authenticator) composed with the **Keycloak**
/// provisioning adapter at `idp_url`, scoped to `tenant`. Returns the router, the wired
/// provisioning runtime and the owner pool.
async fn admin_app(
    tenant: &'static str,
    idp_url: &'static str,
) -> (Router, IdentityRuntime, sqlx::PgPool) {
    let owner = owner_pool().await;
    let src: HashMap<&'static str, &'static str> = HashMap::from([
        ("SUTRA_ENV", "development"),
        ("DATABASE_URL", runtime_db_url()),
        ("SUTRA_SESSION_STORE", "postgres"),
        ("SUTRA_IDEMPOTENCY_STORE", "postgres"),
        ("SUTRA_AUDIT_SINK", "postgres"),
        ("SUTRA_DEV_TENANT_ID", tenant),
        ("SUTRA_SWEEPERS", "false"),
        ("SUTRA_INBOX_CONSUMER", "false"),
        ("SUTRA_APPROVAL_MODE", "auto"),
        ("SUTRA_IDP_PROVISIONING", "keycloak"),
        ("SUTRA_IDP_URL", idp_url),
        ("SUTRA_IDP_REALM", REALM),
        ("SUTRA_IDP_CLIENT_ID", "sutra-idp-admin"),
        ("SUTRA_IDP_CLIENT_SECRET", IDP_ADMIN_SECRET),
        ("SUTRA_IDP_TIMEOUT_MS", "3000"),
        ("SUTRA_IDP_AUTO_PROVISION", "false"),
        ("SUTRA_IDP_RETRY_BASE_SECS", "30"),
        ("SUTRA_IDP_RETRY_MAX_SECS", "60"),
        ("SUTRA_IDP_TENANTS", tenant),
    ]);
    let cfg = Config::load(&src).expect("config");
    let ctx = AppContext::from_config(&cfg).expect("context");
    let rt = ctx
        .identity_runtime
        .clone()
        .expect("SUTRA_IDP_PROVISIONING=keycloak wires a runtime");
    assert_eq!(rt.port.kind(), "keycloak");
    sutra_api::idp_wiring::startup_check(&cfg, &rt)
        .await
        .expect("start-up check");
    (router(ctx), rt, owner)
}

/// The same database behind SutraERP's real OIDC sign-in (`SUTRA_AUTH_MODE=keycloak`, client
/// `sutra-web` of the dev realm).
fn oidc_app() -> Router {
    let issuer = leak(format!("{}/realms/{REALM}", kc_url()));
    let src: HashMap<&'static str, &'static str> = HashMap::from([
        ("SUTRA_ENV", "development"),
        ("DATABASE_URL", runtime_db_url()),
        ("SUTRA_SESSION_STORE", "postgres"),
        ("SUTRA_IDEMPOTENCY_STORE", "postgres"),
        ("SUTRA_AUDIT_SINK", "postgres"),
        ("SUTRA_SWEEPERS", "false"),
        ("SUTRA_INBOX_CONSUMER", "false"),
        ("SUTRA_AUTH_MODE", "keycloak"),
        ("SUTRA_KEYCLOAK_URL", kc_url()),
        ("SUTRA_KEYCLOAK_REALM", REALM),
        ("SUTRA_OIDC_ISSUER", issuer),
    ]);
    let cfg = Config::load(&src).expect("config");
    router(AppContext::from_config(&cfg).expect("context"))
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

async fn exec(pool: &sqlx::PgPool, sql: &str, id: Uuid) {
    sutra_kernel::db_context::system(async {
        sqlx::query(sql).bind(id).execute(pool).await.unwrap();
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

fn url_enc(v: &str) -> String {
    let mut out = String::new();
    for b in v.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn names(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| r["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// Keycloak master-realm admin (test set-up and inspection only — the product never uses it)
// ---------------------------------------------------------------------------------------------

struct KcAdmin {
    http: reqwest::Client,
    token: String,
}

impl KcAdmin {
    async fn new() -> Self {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .unwrap();
        let v: Value = http
            .post(format!(
                "{}/realms/master/protocol/openid-connect/token",
                kc_url()
            ))
            .form(&[
                ("grant_type", "password"),
                ("client_id", "admin-cli"),
                ("username", "admin"),
                ("password", "admin"),
            ])
            .send()
            .await
            .expect("master token")
            .json()
            .await
            .unwrap();
        Self {
            http,
            token: s(&v, "access_token"),
        }
    }

    async fn req(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut r = self
            .http
            .request(method, format!("{}/admin/realms/{REALM}{path}", kc_url()))
            .bearer_auth(&self.token);
        if let Some(b) = body {
            r = r.json(&b);
        }
        let res = r.send().await.expect("keycloak admin call");
        let status = res.status().as_u16();
        let text = res.text().await.unwrap_or_default();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    async fn get(&self, path: &str) -> Value {
        let (st, v) = self.req(reqwest::Method::GET, path, None).await;
        assert!(st < 300, "GET {path}: {st} {v}");
        v
    }

    async fn user(&self, id: &str) -> Value {
        self.get(&format!("/users/{id}")).await
    }

    async fn users_by_email(&self, email: &str) -> Vec<Value> {
        let v = self
            .get(&format!("/users?email={}&exact=true", url_enc(email)))
            .await;
        v.as_array().cloned().unwrap_or_default()
    }

    async fn users_by_username(&self, username: &str) -> Vec<Value> {
        let v = self
            .get(&format!("/users?username={}&exact=true", url_enc(username)))
            .await;
        v.as_array().cloned().unwrap_or_default()
    }

    async fn realm_roles(&self, id: &str) -> Vec<String> {
        names(&self.get(&format!("/users/{id}/role-mappings/realm")).await)
    }

    async fn group_paths(&self, id: &str) -> Vec<String> {
        self.get(&format!("/users/{id}/groups"))
            .await
            .as_array()
            .unwrap()
            .iter()
            .map(|g| s(g, "path"))
            .collect()
    }

    async fn sessions(&self, id: &str) -> usize {
        self.get(&format!("/users/{id}/sessions"))
            .await
            .as_array()
            .map_or(0, Vec::len)
    }

    /// Activation as the user would do it from the e-mailed link: the e-mail is verified (the
    /// e-mail step itself needs SMTP — BLOCKED_EXTERNAL) and an initial temporary password is set,
    /// so `UPDATE_PASSWORD` stays pending and is completed on Keycloak's own page at sign-in.
    async fn activate(&self, id: &str, initial_password: &str) {
        let mut rep = self.user(id).await;
        rep["emailVerified"] = json!(true);
        let actions: Vec<Value> = rep["requiredActions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|a| *a != "VERIFY_EMAIL")
            .collect();
        rep["requiredActions"] = Value::Array(actions);
        let (st, v) = self
            .req(reqwest::Method::PUT, &format!("/users/{id}"), Some(rep))
            .await;
        assert!(st < 300, "update user: {st} {v}");
        let (st, v) = self
            .req(
                reqwest::Method::PUT,
                &format!("/users/{id}/reset-password"),
                Some(json!({"type": "password", "value": initial_password, "temporary": true})),
            )
            .await;
        assert!(st < 300, "reset password: {st} {v}");
    }
}

// ---------------------------------------------------------------------------------------------
// Browser: SutraERP OIDC start → Keycloak login pages → SutraERP callback
// ---------------------------------------------------------------------------------------------

/// Minimal cookie jar keyed by name (one Keycloak host).
#[derive(Default)]
struct Jar(BTreeMap<String, String>);

impl Jar {
    fn absorb<'a>(&mut self, set_cookies: impl Iterator<Item = &'a str>) {
        for c in set_cookies {
            let first = c.split(';').next().unwrap_or("");
            let Some((n, v)) = first.split_once('=') else {
                continue;
            };
            let lower = c.to_ascii_lowercase();
            if v.is_empty() || lower.contains("max-age=0") || lower.contains("01-jan-1970") {
                self.0.remove(n.trim());
            } else {
                self.0.insert(n.trim().to_owned(), v.to_owned());
            }
        }
    }

    fn header(&self) -> String {
        self.0
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Value of attribute `name` inside one HTML tag.
fn attr(tag: &str, name: &str) -> Option<String> {
    let pat = format!(" {name}=\"");
    let i = tag.find(&pat)? + pat.len();
    let end = tag[i..].find('"')?;
    Some(tag[i..i + end].replace("&amp;", "&"))
}

/// One `<input>` of a form: (name, type, value).
type Field = (String, String, String);

/// `(action, inputs)` of the first form of a Keycloak page.
fn first_form(html: &str) -> Option<(String, Vec<Field>)> {
    let start = html.find("<form")?;
    let rest = &html[start..];
    let open_end = rest.find('>')?;
    let action = attr(&rest[..open_end], "action")?;
    let body = &rest[..rest.find("</form>").unwrap_or(rest.len())];
    let mut fields = vec![];
    let mut cursor = body;
    while let Some(i) = cursor.find("<input") {
        let tag_end = cursor[i..].find('>').map_or(cursor.len(), |e| i + e);
        let tag = &cursor[i..tag_end];
        if let Some(n) = attr(tag, "name") {
            fields.push((
                n,
                attr(tag, "type").unwrap_or_else(|| "text".into()),
                attr(tag, "value").unwrap_or_default(),
            ));
        }
        cursor = &cursor[tag_end..];
    }
    Some((action, fields))
}

/// Keycloak's username + password form (not a required-action page).
fn is_login_form(html: &str) -> bool {
    html.contains("name=\"username\"")
        && html.contains("name=\"password\"")
        && !html.contains("password-new")
}

/// Text of a page without tags (for assertion messages).
fn visible_text(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Outcome of one browser sign-in attempt.
#[derive(Debug)]
enum SignIn {
    /// SutraERP accepted the callback: the session cookie (`name=value`).
    Session(String),
    /// Keycloak did not issue a code; the text of its last page.
    Refused(String),
}

struct Browser {
    http: reqwest::Client,
    jar: Jar,
}

impl Browser {
    fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()
                .unwrap(),
            jar: Jar::default(),
        }
    }

    async fn kc(&mut self, req: reqwest::RequestBuilder) -> (u16, Option<String>, String) {
        let res = req
            .header(reqwest::header::COOKIE, self.jar.header())
            .send()
            .await
            .expect("keycloak page");
        self.jar.absorb(
            res.headers()
                .get_all(reqwest::header::SET_COOKIE)
                .iter()
                .filter_map(|v| v.to_str().ok()),
        );
        let status = res.status().as_u16();
        let location = res
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        (status, location, res.text().await.unwrap_or_default())
    }

    /// Signs in through SutraERP's `/api/v1/auth/oidc/start` → Keycloak → `/callback`, filling
    /// Keycloak's pages: the login form, then any required action (`UPDATE_PASSWORD`, profile).
    async fn sign_in(
        &mut self,
        app: &Router,
        username: &str,
        password: &str,
        new_password: &str,
    ) -> SignIn {
        // 1. SutraERP: state + PKCE + browser-binding cookie, 303 to Keycloak.
        let res = app
            .clone()
            .oneshot(
                Request::get("/api/v1/auth/oidc/start")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SEE_OTHER, "oidc start");
        let binding = res.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let mut location = res.headers()[header::LOCATION].to_str().unwrap().to_owned();
        assert!(location.starts_with(kc_url()), "{location}");
        // 2. Keycloak pages: follow redirects, fill each form until Keycloak redirects to the
        //    callback, or refuses (the login form comes back after the credentials were posted).
        let mut page = String::new();
        let mut callback: Option<String> = None;
        let mut next = Some(self.http.get(location.as_str()));
        for _ in 0..16 {
            let Some(req) = next.take() else {
                break;
            };
            let (status, loc, body) = self.kc(req).await;
            if (300..400).contains(&status) {
                let Some(l) = loc else {
                    break;
                };
                if l.contains("/api/v1/auth/oidc/callback") {
                    callback = Some(l);
                    break;
                }
                location = if l.starts_with("http") {
                    l
                } else {
                    format!("{}{l}", kc_url())
                };
                next = Some(self.http.get(location.as_str()));
                continue;
            }
            let posted_credentials = is_login_form(&page);
            page = body;
            if posted_credentials && is_login_form(&page) {
                break;
            }
            let Some((action, fields)) = first_form(&page) else {
                break;
            };
            let mut form: Vec<(String, String)> = vec![];
            for (name, kind, value) in fields {
                let v = match name.as_str() {
                    "username" => username.to_owned(),
                    "password" => password.to_owned(),
                    "password-new" | "password-confirm" => new_password.to_owned(),
                    "firstName" | "lastName" if value.is_empty() => "Test".to_owned(),
                    "cancel-aia" => continue,
                    _ if kind == "checkbox" || kind == "submit" => continue,
                    _ => value,
                };
                form.push((name, v));
            }
            next = Some(self.http.post(action).form(&form));
        }
        let Some(callback) = callback else {
            return SignIn::Refused(visible_text(&page));
        };
        // 3. SutraERP callback: code exchange, ID-token validation, claim mapping, session.
        let path = &callback[callback.find("/api/v1/auth/oidc/callback").unwrap()..];
        let res = app
            .clone()
            .oneshot(
                Request::get(path)
                    .header(header::COOKIE, &binding)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SEE_OTHER, "callback");
        let to = res.headers()[header::LOCATION].to_str().unwrap().to_owned();
        assert!(
            !to.contains("authError"),
            "SutraERP refused the sign-in: {to}"
        );
        let session = res
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(|c| c.split(';').next().unwrap_or("").to_owned())
            .find(|c| c.contains("sutra_session=") && !c.ends_with('='))
            .expect("session cookie");
        SignIn::Session(session)
    }
}

async fn session_info(app: &Router, cookie: &str) -> (StatusCode, Value) {
    let (s, _, b) = send(
        app,
        Request::get("/api/v1/auth/session")
            .header(header::COOKIE, cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    (s, b)
}

fn roles_of(info: &Value) -> Vec<String> {
    let mut v: Vec<String> = info["principal"]["roles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap().to_owned())
        .collect();
    v.sort();
    v
}

/// `(status, body)` of a [`send`] result.
trait StatusBody {
    fn into_sb(self) -> (StatusCode, Value);
}

impl StatusBody for (StatusCode, axum::http::HeaderMap, Value) {
    fn into_sb(self) -> (StatusCode, Value) {
        (self.0, self.2)
    }
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn provisioned_student_signs_in_and_suspension_locks_them_out() {
    let _g = SERIAL.lock().await;
    let (app, rt, pool) = admin_app(T_LOGIN, kc_url()).await;
    // The dev realm's service account is least privilege (no view-realm / view-clients / …).
    rt.port.check().await.expect("least-privilege admin client");
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = support::b3::academic_setup(&app, &admin, T_LOGIN, "Live IdP login").await;
    let tag = Uuid::now_v7().simple().to_string();
    let email = format!("asha.{tag}@student.example.edu");
    let st = seeded(&pool, &setup, "Asha", Some(&email)).await;
    let kc = KcAdmin::new().await;

    // Provision → Keycloak account in the tenant / institution group with the student role.
    let (code, v) = call(
        &app,
        &admin,
        "POST",
        &format!("{ID}/provision"),
        json!({"subjectType": "student", "subjectId": st.student_id}),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{v}");
    let link_id = s(&v, "id");
    sync(&pool, &rt).await;
    let l = link(&pool, T_LOGIN, st.student_id).await;
    assert_eq!(l["sync_status"], "synced", "{l}");
    assert_eq!(l["idp"], "keycloak");
    let uid = s(&l, "idp_user_id");
    let u = kc.user(&uid).await;
    assert_eq!(u["username"], email.as_str());
    assert_eq!(u["enabled"], true);
    let ra: Vec<String> = serde_json::from_value(u["requiredActions"].clone()).unwrap();
    assert!(
        ra.contains(&"VERIFY_EMAIL".to_owned()) && ra.contains(&"UPDATE_PASSWORD".to_owned()),
        "{ra:?}"
    );
    assert!(kc.realm_roles(&uid).await.contains(&"student".to_owned()));
    assert_eq!(
        kc.group_paths(&uid).await,
        vec![format!(
            "/sutra-tenant-{T_LOGIN}/institution-{}",
            setup.institution_id
        )]
    );
    let inst_group = kc
        .get(&format!(
            "/group-by-path/sutra-tenant-{T_LOGIN}/institution-{}",
            setup.institution_id
        ))
        .await;
    assert_eq!(inst_group["attributes"]["tenant_id"], json!([T_LOGIN]));
    assert_eq!(
        inst_group["attributes"]["institution_id"],
        json!([setup.institution_id.to_string()])
    );
    // The user carries no tenant attribute of its own: the claim must come from the group.
    assert!(u["attributes"]["tenant_id"].is_null(), "{u}");
    // Idempotent: another sync creates nothing.
    sync(&pool, &rt).await;
    assert_eq!(kc.users_by_email(&email).await.len(), 1);

    // Activation + real OIDC sign-in (Keycloak login page, UPDATE_PASSWORD page, code exchange).
    kc.activate(&uid, "Initial-pass-1").await;
    let oidc = oidc_app();
    let password = format!("Asha-{}", &tag[..12]);
    let mut browser = Browser::new();
    let cookie = match browser
        .sign_in(&oidc, &email, "Initial-pass-1", &password)
        .await
    {
        SignIn::Session(c) => c,
        SignIn::Refused(p) => panic!("activated student could not sign in: {p}"),
    };
    let (code, info) = session_info(&oidc, &cookie).await;
    assert_eq!(code, StatusCode::OK, "{info}");
    assert_eq!(info["principal"]["subject"], uid.as_str());
    assert_eq!(
        info["principal"]["scope"]["tenantId"], T_LOGIN,
        "tenant claim resolved from the group attribute: {info}"
    );
    assert_eq!(
        info["principal"]["scope"]["institutionId"],
        setup.institution_id.to_string(),
        "{info}"
    );
    assert_eq!(roles_of(&info), vec!["student".to_owned()]);
    assert!(kc.user(&uid).await["requiredActions"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(kc.sessions(&uid).await >= 1, "Keycloak session open");

    // Role mapping (ERP role → extra realm role) + a granted ERP role, applied by the
    // least-privilege client via role-mappings/realm/available, effective at the next sign-in.
    let lms_role = format!("lms-student-{}", &tag[..8]);
    let (st_code, v) = kc
        .req(
            reqwest::Method::POST,
            "/roles",
            Some(json!({"name": lms_role, "description": "S4-idp live test"})),
        )
        .await;
    assert!(st_code < 300, "{v}");
    let (code, m) = call(
        &app,
        &admin,
        "POST",
        "/api/v1/platform/identity-role-mappings",
        json!({"erpRole": "student", "targetKind": "realm_role", "target": lms_role}),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{m}");
    let (code, v) = call(
        &app,
        &admin,
        "POST",
        &format!("{ID}/{link_id}/role-requests"),
        json!({"grant": ["faculty"], "reason": "teaching assistant"}),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{v}");
    assert_eq!(v["status"], "applied", "non-privileged grant: {v}");
    sync(&pool, &rt).await;
    let l = link(&pool, T_LOGIN, st.student_id).await;
    assert_eq!(l["sync_status"], "synced", "{l}");
    let roles = kc.realm_roles(&uid).await;
    for r in ["student", "faculty", lms_role.as_str()] {
        assert!(roles.iter().any(|x| x == r), "{r} mapped: {roles:?}");
    }
    let cookie = match browser.sign_in(&oidc, &email, &password, &password).await {
        SignIn::Session(c) => c,
        SignIn::Refused(p) => panic!("second sign-in refused: {p}"),
    };
    let (code, info) = session_info(&oidc, &cookie).await;
    assert_eq!(code, StatusCode::OK, "{info}");
    assert_eq!(
        roles_of(&info),
        vec!["faculty".to_owned(), "student".to_owned()],
        "{info}"
    );

    // ERP suspension → disabled at Keycloak, Keycloak + SutraERP sessions ended, sign-in refused.
    let sid = st.student_id;
    let (code, stu) = send(&app, get(&admin, &format!("{S}/students/{sid}")))
        .await
        .into_sb();
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
    .into_sb();
    assert_eq!(code, StatusCode::OK, "{body}");
    events(&pool, &rt).await;
    assert_eq!(
        link(&pool, T_LOGIN, sid).await["desired_state"],
        "suspended"
    );
    sync(&pool, &rt).await;
    let l = link(&pool, T_LOGIN, sid).await;
    assert_eq!(l["sync_status"], "synced", "{l}");
    assert_eq!(kc.user(&uid).await["enabled"], false);
    assert_eq!(kc.sessions(&uid).await, 0, "Keycloak sessions revoked");
    assert_eq!(
        session_info(&oidc, &cookie).await.0,
        StatusCode::UNAUTHORIZED,
        "SutraERP session revoked"
    );
    match Browser::new()
        .sign_in(&oidc, &email, &password, &password)
        .await
    {
        SignIn::Refused(page) => eprintln!("suspended sign-in refused by Keycloak: {page}"),
        SignIn::Session(_) => panic!("a suspended student signed in"),
    }

    // Restoration → the student can sign in again.
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
    .into_sb();
    assert_eq!(code, StatusCode::OK, "{body}");
    events(&pool, &rt).await;
    sync(&pool, &rt).await;
    assert_eq!(kc.user(&uid).await["enabled"], true);
    let back = Browser::new()
        .sign_in(&oidc, &email, &password, &password)
        .await;
    assert!(matches!(back, SignIn::Session(_)), "{back:?}");
}

#[tokio::test]
async fn duplicate_identities_are_prevented_at_keycloak() {
    let _g = SERIAL.lock().await;
    let (app, rt, pool) = admin_app(T_DUP, kc_url()).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = support::b3::academic_setup(&app, &admin, T_DUP, "Live IdP duplicates").await;
    let kc = KcAdmin::new().await;
    let tag = Uuid::now_v7().simple().to_string();

    // The person already has a Keycloak account (same e-mail, other username): adopted.
    let email = format!("ravi.{tag}@student.example.edu");
    let a = seeded(&pool, &setup, "Ravi", Some(&email)).await;
    let legacy = rt
        .port
        .create_user(&NewIdpUser {
            username: format!("legacy-{tag}"),
            email: Some(email.clone()),
            first_name: Some("Ravi".into()),
            last_name: None,
            enabled: true,
            required_actions: vec![],
        })
        .await
        .expect("pre-existing account");
    // Keycloak itself refuses a second account with a taken username.
    let dup = rt
        .port
        .create_user(&NewIdpUser {
            username: format!("legacy-{tag}"),
            email: None,
            first_name: None,
            last_name: None,
            enabled: true,
            required_actions: vec![],
        })
        .await;
    assert!(
        matches!(dup, Err(sutra_kernel::error::KernelError::Conflict(_))),
        "{dup:?}"
    );
    let (code, v) = call(
        &app,
        &admin,
        "POST",
        &format!("{ID}/provision"),
        json!({"subjectType": "student", "subjectId": a.student_id}),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{v}");
    let (code, again) = call(
        &app,
        &admin,
        "POST",
        &format!("{ID}/provision"),
        json!({"subjectType": "student", "subjectId": a.student_id}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{again}");
    assert_eq!(v["id"], again["id"], "one identity per subject");
    sync(&pool, &rt).await;
    let l = link(&pool, T_DUP, a.student_id).await;
    assert_eq!(l["sync_status"], "synced", "{l}");
    assert_eq!(l["idp_user_id"], legacy.as_str(), "adopted, not duplicated");
    sync(&pool, &rt).await;
    assert_eq!(kc.users_by_email(&email).await.len(), 1);
    assert_eq!(
        kc.group_paths(&legacy).await,
        vec![format!(
            "/sutra-tenant-{T_DUP}/institution-{}",
            setup.institution_id
        )]
    );

    // A second student with the same e-mail is refused before Keycloak is touched.
    let b = seeded(&pool, &setup, "Ravi B", Some(&email.to_uppercase())).await;
    let (code, v) = call(
        &app,
        &admin,
        "POST",
        &format!("{ID}/provision"),
        json!({"subjectType": "student", "subjectId": b.student_id}),
    )
    .await;
    assert_eq!(code, StatusCode::CONFLICT, "{v}");

    // An account that belongs to another tenant is never adopted.
    let other_email = format!("chitra.{tag}@student.example.edu");
    let c = seeded(&pool, &setup, "Chitra", Some(&other_email)).await;
    let foreign = rt
        .port
        .create_user(&NewIdpUser {
            username: other_email.clone(),
            email: None,
            first_name: Some("Chitra".into()),
            last_name: None,
            enabled: true,
            required_actions: vec![],
        })
        .await
        .unwrap();
    let g = rt
        .port
        .ensure_scope_group(&ScopeGroup {
            tenant_id: T_FOREIGN.parse().unwrap(),
            institution_id: None,
        })
        .await
        .unwrap();
    rt.port.join_group(&foreign, &g).await.unwrap();
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
    assert_eq!(
        kc.group_paths(&foreign).await,
        vec![format!("/sutra-tenant-{T_FOREIGN}")],
        "the foreign account was left alone"
    );
    assert_eq!(
        kc.users_by_username(&other_email).await.len(),
        1,
        "no second account"
    );
}

/// TCP proxy in front of Keycloak that can drop every connection (an outage the adapter sees
/// exactly as an unreachable server).
struct Outage {
    url: &'static str,
    down: tokio::sync::watch::Sender<bool>,
}

async fn outage_proxy(target: String) -> Outage {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        loop {
            let Ok((mut inbound, _)) = listener.accept().await else {
                break;
            };
            if *rx.borrow() {
                drop(inbound);
                continue;
            }
            let target = target.clone();
            let mut down = rx.clone();
            tokio::spawn(async move {
                let Ok(mut outbound) = tokio::net::TcpStream::connect(&target).await else {
                    return;
                };
                tokio::select! {
                    _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => {}
                    _ = down.wait_for(|d| *d) => {}
                }
            });
        }
    });
    Outage {
        url: leak(format!("http://127.0.0.1:{port}")),
        down: tx,
    }
}

#[tokio::test]
async fn sync_retries_while_keycloak_is_unreachable() {
    let _g = SERIAL.lock().await;
    let target = kc_url()
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap()
        .to_owned();
    let proxy = outage_proxy(target).await;
    let (app, rt, pool) = admin_app(T_RETRY, proxy.url).await;
    rt.port.check().await.expect("reachable through the proxy");
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = support::b3::academic_setup(&app, &admin, T_RETRY, "Live IdP retry").await;
    let st = seeded(&pool, &setup, "Meena", None).await;
    let (code, v) = call(
        &app,
        &admin,
        "POST",
        &format!("{ID}/provision"),
        json!({"subjectType": "student", "subjectId": st.student_id}),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{v}");
    let username = s(&v, "username");

    // Keycloak unreachable → failed, backed off, nothing created.
    proxy.down.send_replace(true);
    sync(&pool, &rt).await;
    let l = link(&pool, T_RETRY, st.student_id).await;
    assert_eq!(l["sync_status"], "failed", "{l}");
    assert_eq!(l["attempts"], 1);
    eprintln!(
        "sync error while Keycloak was down: {}",
        s(&l, "last_error")
    );
    // Back: an immediate tick does not retry before the back-off.
    proxy.down.send_replace(false);
    sync(&pool, &rt).await;
    assert_eq!(link(&pool, T_RETRY, st.student_id).await["attempts"], 1);
    let kc = KcAdmin::new().await;
    assert!(kc.users_by_username(&username).await.is_empty());
    // Due again → converges to exactly one account.
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
    sync(&pool, &rt).await;
    let found = kc.users_by_username(&username).await;
    assert_eq!(found.len(), 1, "exactly one account");
    assert_eq!(l["idp_user_id"], found[0]["id"]);
}
