//! Keycloak 26 Admin REST adapter of [`IdentityProvisioningPort`] (Part 2 · S4-idp).
//!
//! Authenticates as a **confidential service-account client** (`client_credentials`, client
//! secret in the request body over the back channel) and caches the access token until shortly
//! before it expires. The client is expected to hold exactly the least-privilege set of
//! `realm-management` client roles below; [`KeycloakAdmin::check`] (run at start-up, refusing
//! production on failure) reads them from the token and rejects an over- or under-privileged
//! client:
//!
//! | `realm-management` role | why |
//! |---|---|
//! | `manage-users` | create users, enable / disable, required actions + actions e-mail, logout, role and group membership, create the tenant / institution groups |
//! | `view-users` | read users, their role mappings and the roles available to map |
//! | `query-users` | search users by username / e-mail |
//! | `query-groups` | look groups up by path |
//! | `query-clients` *(optional)* | resolve a client id for client-role mappings |
//!
//! Anything else (`realm-admin`, `manage-realm`, `manage-clients`, `view-clients` — which can read
//! client secrets — `view-realm`, `impersonation`, `manage-authorization`, …) or any SutraERP realm
//! role on the service account is refused.
//!
//! Endpoints (Keycloak Admin REST API, <https://www.keycloak.org/docs-api/latest/rest-api/>):
//! `POST /realms/{r}/protocol/openid-connect/token`; `GET|POST /admin/realms/{r}/users`;
//! `GET|PUT /users/{id}`; `PUT /users/{id}/execute-actions-email`; `POST /users/{id}/logout`;
//! `GET|POST|DELETE /users/{id}/role-mappings/realm` (+ `/available`);
//! `GET|POST|DELETE /users/{id}/role-mappings/clients/{client}` (+ `/available`);
//! `GET /users/{id}/groups`; `PUT|DELETE /users/{id}/groups/{group}`;
//! `GET /group-by-path/{path}`; `POST /groups`; `POST /groups/{id}/children`;
//! `GET /clients?clientId=`.

use base64::Engine;
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use sutra_contracts::platform::identity::{
    IdentityProvisioningPort, IdpGroup, IdpUser, NewIdpUser, RequiredAction, ScopeGroup,
};
use sutra_kernel::auth::Role;
use sutra_kernel::error::KernelError;
use tokio::sync::Mutex;

/// `realm-management` roles the admin client must hold.
pub const REQUIRED_ADMIN_ROLES: [&str; 4] =
    ["manage-users", "view-users", "query-users", "query-groups"];
/// `realm-management` roles the admin client may additionally hold.
pub const OPTIONAL_ADMIN_ROLES: [&str; 1] = ["query-clients"];

/// Settings of the admin client (`SUTRA_IDP_*`).
#[derive(Clone, Debug)]
pub struct KeycloakAdminSettings {
    /// Server root reachable from the API, e.g. `https://sso.example.edu` or
    /// `http://keycloak:8180` (no `/realms/...`).
    pub base_url: String,
    pub realm: String,
    pub client_id: String,
    pub client_secret: String,
    pub timeout: Duration,
    /// Honour `HTTP(S)_PROXY` / `NO_PROXY` (default `true`).
    pub system_proxy: bool,
    /// Validity of an "execute actions" e-mail link.
    pub actions_email_lifespan: Duration,
    /// Client + redirect the user lands on after completing e-mailed actions.
    pub actions_client_id: Option<String>,
    pub actions_redirect_uri: Option<String>,
}

impl KeycloakAdminSettings {
    pub fn new(base_url: &str, realm: &str, client_id: &str, client_secret: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            realm: realm.to_owned(),
            client_id: client_id.to_owned(),
            client_secret: client_secret.to_owned(),
            timeout: Duration::from_secs(10),
            system_proxy: true,
            actions_email_lifespan: Duration::from_secs(72 * 3600),
            actions_client_id: None,
            actions_redirect_uri: None,
        }
    }
}

/// Percent-encodes one path segment / query value.
fn enc(v: &str) -> String {
    url::form_urlencoded::byte_serialize(v.as_bytes()).collect()
}

/// Claims of the admin access token the adapter reads (unverified: the token comes straight
/// from the token endpoint over the back channel and is only used to report its own grants).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AdminTokenGrants {
    pub exp: Option<i64>,
    pub realm_management: Vec<String>,
    pub realm_roles: Vec<String>,
}

/// Decodes the payload of a compact JWS without verifying it (see [`AdminTokenGrants`]).
pub fn token_grants(token: &str) -> Result<AdminTokenGrants, KernelError> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| KernelError::Unavailable("admin token is not a JWT".into()))?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|_| KernelError::Unavailable("admin token payload is not base64url".into()))?;
    let v: Value = serde_json::from_slice(&bytes)
        .map_err(|_| KernelError::Unavailable("admin token payload is not JSON".into()))?;
    let strings = |v: Option<&Value>| -> Vec<String> {
        v.and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    Ok(AdminTokenGrants {
        exp: v.get("exp").and_then(Value::as_i64),
        realm_management: strings(v.pointer("/resource_access/realm-management/roles")),
        realm_roles: strings(v.pointer("/realm_access/roles")),
    })
}

/// Least-privilege rule for the admin client (pure; unit-tested).
pub fn check_admin_grants(g: &AdminTokenGrants) -> Result<(), String> {
    let missing: Vec<&str> = REQUIRED_ADMIN_ROLES
        .iter()
        .copied()
        .filter(|r| !g.realm_management.iter().any(|h| h == r))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "the identity admin client lacks realm-management role(s) {}",
            missing.join(", ")
        ));
    }
    let excess: Vec<&str> = g
        .realm_management
        .iter()
        .map(String::as_str)
        .filter(|r| !REQUIRED_ADMIN_ROLES.contains(r) && !OPTIONAL_ADMIN_ROLES.contains(r))
        .collect();
    if !excess.is_empty() {
        return Err(format!(
            "the identity admin client is over-privileged: realm-management role(s) {} are not \
             allowed (least privilege: {} [+ {}])",
            excess.join(", "),
            REQUIRED_ADMIN_ROLES.join(", "),
            OPTIONAL_ADMIN_ROLES.join(", ")
        ));
    }
    let erp: Vec<&str> = g
        .realm_roles
        .iter()
        .map(String::as_str)
        .filter(|r| Role::parse(r).is_some())
        .collect();
    if !erp.is_empty() {
        return Err(format!(
            "the identity admin client's service account holds SutraERP role(s) {}",
            erp.join(", ")
        ));
    }
    Ok(())
}

/// Keycloak Admin REST client.
pub struct KeycloakAdmin {
    settings: KeycloakAdminSettings,
    http: reqwest::Client,
    token: Mutex<Option<(String, Instant)>>,
}

impl KeycloakAdmin {
    pub fn new(settings: KeycloakAdminSettings) -> Result<Self, KernelError> {
        let mut builder = reqwest::Client::builder()
            .timeout(settings.timeout)
            .redirect(reqwest::redirect::Policy::none());
        if !settings.system_proxy {
            builder = builder.no_proxy();
        }
        // Part 2 · S5-capacity: Keycloak behind the institution's own CA.
        let builder = crate::tls::with_extra_roots(builder).map_err(KernelError::internal_msg)?;
        Ok(Self {
            http: builder.build().map_err(KernelError::internal)?,
            settings,
            token: Mutex::new(None),
        })
    }

    pub fn settings(&self) -> &KeycloakAdminSettings {
        &self.settings
    }

    fn admin(&self, path: &str) -> String {
        format!(
            "{}/admin/realms/{}{path}",
            self.settings.base_url,
            enc(&self.settings.realm)
        )
    }

    async fn fetch_token(&self) -> Result<(String, Instant), KernelError> {
        let s = &self.settings;
        let url = format!(
            "{}/realms/{}/protocol/openid-connect/token",
            s.base_url,
            enc(&s.realm)
        );
        let res = self
            .http
            .post(&url)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", s.client_id.as_str()),
                ("client_secret", s.client_secret.as_str()),
            ])
            .send()
            .await
            .map_err(|e| KernelError::Unavailable(format!("identity provider token: {e}")))?;
        let status = res.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::BAD_REQUEST {
            return Err(KernelError::Forbidden(format!(
                "the identity admin client was refused by the token endpoint ({status}): check \
                 SUTRA_IDP_CLIENT_ID / SUTRA_IDP_CLIENT_SECRET and that service accounts are \
                 enabled"
            )));
        }
        if !status.is_success() {
            return Err(KernelError::Unavailable(format!(
                "identity provider token endpoint answered {status}"
            )));
        }
        let body: Value = res
            .json()
            .await
            .map_err(|e| KernelError::Unavailable(format!("identity provider token: {e}")))?;
        let token = body
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| KernelError::Unavailable("token response without access_token".into()))?
            .to_owned();
        let ttl = body.get("expires_in").and_then(Value::as_u64).unwrap_or(60);
        // Refresh 30 s (or half the lifetime) before expiry.
        let margin = (ttl / 2).min(30);
        Ok((
            token,
            Instant::now() + Duration::from_secs(ttl.saturating_sub(margin)),
        ))
    }

    async fn token(&self, force: bool) -> Result<String, KernelError> {
        let mut guard = self.token.lock().await;
        if !force {
            if let Some((t, until)) = guard.as_ref() {
                if Instant::now() < *until {
                    return Ok(t.clone());
                }
            }
        }
        let fresh = self.fetch_token().await?;
        let t = fresh.0.clone();
        *guard = Some(fresh);
        Ok(t)
    }

    /// One admin call; retries once with a fresh token on `401`. Returns status + body.
    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(StatusCode, reqwest::header::HeaderMap, Value), KernelError> {
        let url = self.admin(path);
        let mut force = false;
        loop {
            let token = self.token(force).await?;
            let mut req = self.http.request(method.clone(), &url).bearer_auth(&token);
            if let Some(b) = body {
                req = req.json(b);
            }
            let res = req.send().await.map_err(|e| {
                KernelError::Unavailable(format!("identity provider {method} {path}: {e}"))
            })?;
            let status = res.status();
            if status == StatusCode::UNAUTHORIZED && !force {
                force = true;
                continue;
            }
            let headers = res.headers().clone();
            let text = res.text().await.unwrap_or_default();
            let value = if text.trim().is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&text).unwrap_or(Value::String(text))
            };
            return Ok((status, headers, value));
        }
    }

    /// Like [`Self::call`] but maps every non-success status to an error.
    async fn ok(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, KernelError> {
        let (status, _, v) = self.call(method.clone(), path, body).await?;
        if status.is_success() {
            return Ok(v);
        }
        Err(status_error(status, &format!("{method} {path}"), &v))
    }

    async fn client_uuid(&self, client_id: &str) -> Result<String, KernelError> {
        let v = self
            .ok(
                Method::GET,
                &format!("/clients?clientId={}&search=false", enc(client_id)),
                None,
            )
            .await?;
        v.as_array()
            .and_then(|a| {
                a.iter()
                    .find(|c| c.get("clientId").and_then(Value::as_str) == Some(client_id))
            })
            .and_then(|c| c.get("id").and_then(Value::as_str))
            .map(str::to_owned)
            .ok_or_else(|| KernelError::Conflict(format!("no client {client_id:?} in the realm")))
    }

    /// Role representations (`id`, `name`) of `names` taken from `listing` (an array of roles).
    fn pick_roles(listing: &Value, names: &[String]) -> Result<Vec<Value>, KernelError> {
        let roles = listing.as_array().cloned().unwrap_or_default();
        let mut out = vec![];
        for n in names {
            let r = roles
                .iter()
                .find(|r| r.get("name").and_then(Value::as_str) == Some(n.as_str()))
                .ok_or_else(|| {
                    KernelError::Conflict(format!(
                        "role {n:?} does not exist (or cannot be mapped)"
                    ))
                })?;
            out.push(json!({
                "id": r.get("id").cloned().unwrap_or(Value::Null),
                "name": n,
            }));
        }
        Ok(out)
    }

    async fn find_group_child(
        &self,
        parent: &str,
        name: &str,
    ) -> Result<Option<String>, KernelError> {
        let v = self
            .ok(
                Method::GET,
                &format!(
                    "/groups/{}/children?search={}&exact=true&briefRepresentation=true&max=50",
                    enc(parent),
                    enc(name)
                ),
                None,
            )
            .await?;
        Ok(v.as_array().and_then(|a| {
            a.iter()
                .find(|g| g.get("name").and_then(Value::as_str) == Some(name))
                .and_then(|g| g.get("id").and_then(Value::as_str))
                .map(str::to_owned)
        }))
    }
}

/// Maps a Keycloak error answer.
fn status_error(status: StatusCode, what: &str, body: &Value) -> KernelError {
    let detail = body
        .get("errorMessage")
        .or_else(|| body.get("error_description"))
        .or_else(|| body.get("error"))
        .and_then(Value::as_str)
        .unwrap_or("");
    match status {
        StatusCode::CONFLICT => {
            KernelError::Conflict(format!("identity provider: {what}: conflict {detail}"))
        }
        StatusCode::NOT_FOUND => KernelError::NotFound {
            entity: "identity provider resource",
        },
        StatusCode::FORBIDDEN => KernelError::Forbidden(format!(
            "identity provider refused {what} (admin client privileges?) {detail}"
        )),
        StatusCode::BAD_REQUEST => {
            KernelError::BadRequest(format!("identity provider rejected {what}: {detail}"))
        }
        s => KernelError::Unavailable(format!("identity provider {what}: {s} {detail}")),
    }
}

fn user_from(v: &Value) -> Option<IdpUser> {
    Some(IdpUser {
        id: v.get("id")?.as_str()?.to_owned(),
        username: v.get("username")?.as_str()?.to_owned(),
        email: v.get("email").and_then(Value::as_str).map(str::to_owned),
        enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(false),
        email_verified: v
            .get("emailVerified")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        required_actions: v
            .get("requiredActions")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn names(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| r.get("name").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Id of a created resource from its `Location` header.
fn location_id(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get(reqwest::header::LOCATION)?
        .to_str()
        .ok()?
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
}

#[async_trait::async_trait]
impl IdentityProvisioningPort for KeycloakAdmin {
    fn kind(&self) -> &'static str {
        "keycloak"
    }

    async fn check(&self) -> Result<(), KernelError> {
        let token = self.token(true).await?;
        check_admin_grants(&token_grants(&token)?).map_err(KernelError::Forbidden)
    }

    async fn user(&self, id: &str) -> Result<Option<IdpUser>, KernelError> {
        let (status, _, v) = self
            .call(Method::GET, &format!("/users/{}", enc(id)), None)
            .await?;
        match status {
            s if s.is_success() => Ok(user_from(&v)),
            StatusCode::NOT_FOUND => Ok(None),
            s => Err(status_error(s, "GET user", &v)),
        }
    }

    async fn find_by_username(&self, username: &str) -> Result<Option<IdpUser>, KernelError> {
        let v = self
            .ok(
                Method::GET,
                &format!("/users?username={}&exact=true", enc(username)),
                None,
            )
            .await?;
        Ok(v.as_array().and_then(|a| {
            a.iter()
                .filter_map(user_from)
                .find(|u| u.username.eq_ignore_ascii_case(username))
        }))
    }

    async fn find_by_email(&self, email: &str) -> Result<Vec<IdpUser>, KernelError> {
        let v = self
            .ok(
                Method::GET,
                &format!("/users?email={}&exact=true", enc(email)),
                None,
            )
            .await?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(user_from)
                    .filter(|u| {
                        u.email
                            .as_deref()
                            .is_some_and(|e| e.eq_ignore_ascii_case(email))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn create_user(&self, user: &NewIdpUser) -> Result<String, KernelError> {
        let mut rep = json!({
            "username": user.username,
            "enabled": user.enabled,
            "emailVerified": false,
            "requiredActions": user.required_actions.iter().map(RequiredAction::keycloak).collect::<Vec<_>>(),
        });
        if let Some(e) = &user.email {
            rep["email"] = json!(e);
        }
        if let Some(f) = &user.first_name {
            rep["firstName"] = json!(f);
        }
        if let Some(l) = &user.last_name {
            rep["lastName"] = json!(l);
        }
        let (status, headers, v) = self.call(Method::POST, "/users", Some(&rep)).await?;
        if !status.is_success() {
            return Err(status_error(status, "POST user", &v));
        }
        match location_id(&headers) {
            Some(id) => Ok(id),
            None => self
                .find_by_username(&user.username)
                .await?
                .map(|u| u.id)
                .ok_or_else(|| KernelError::Unavailable("created user not found".into())),
        }
    }

    async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), KernelError> {
        let path = format!("/users/{}", enc(id));
        let mut rep = self.ok(Method::GET, &path, None).await?;
        if rep.get("enabled").and_then(Value::as_bool) == Some(enabled) {
            return Ok(());
        }
        rep["enabled"] = json!(enabled);
        self.ok(Method::PUT, &path, Some(&rep)).await.map(|_| ())
    }

    async fn add_required_actions(
        &self,
        id: &str,
        actions: &[RequiredAction],
        send_email: bool,
    ) -> Result<(), KernelError> {
        if actions.is_empty() {
            return Ok(());
        }
        let path = format!("/users/{}", enc(id));
        let mut rep = self.ok(Method::GET, &path, None).await?;
        let mut current: Vec<String> = rep
            .get("requiredActions")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        for a in actions {
            if !current.iter().any(|c| c == a.keycloak()) {
                current.push(a.keycloak().to_owned());
            }
        }
        rep["requiredActions"] = json!(current);
        self.ok(Method::PUT, &path, Some(&rep)).await?;
        if send_email {
            let s = &self.settings;
            let mut q = format!("lifespan={}", s.actions_email_lifespan.as_secs());
            if let Some(c) = &s.actions_client_id {
                q.push_str(&format!("&client_id={}", enc(c)));
            }
            if let Some(r) = &s.actions_redirect_uri {
                q.push_str(&format!("&redirect_uri={}", enc(r)));
            }
            let body = json!(actions
                .iter()
                .map(RequiredAction::keycloak)
                .collect::<Vec<_>>());
            self.ok(
                Method::PUT,
                &format!("{path}/execute-actions-email?{q}"),
                Some(&body),
            )
            .await?;
        }
        Ok(())
    }

    async fn logout(&self, id: &str) -> Result<(), KernelError> {
        self.ok(Method::POST, &format!("/users/{}/logout", enc(id)), None)
            .await
            .map(|_| ())
    }

    async fn realm_roles(&self, id: &str) -> Result<Vec<String>, KernelError> {
        let v = self
            .ok(
                Method::GET,
                &format!("/users/{}/role-mappings/realm", enc(id)),
                None,
            )
            .await?;
        Ok(names(&v))
    }

    async fn add_realm_roles(&self, id: &str, roles: &[String]) -> Result<(), KernelError> {
        if roles.is_empty() {
            return Ok(());
        }
        let base = format!("/users/{}/role-mappings/realm", enc(id));
        let available = self
            .ok(Method::GET, &format!("{base}/available"), None)
            .await?;
        let current = self.ok(Method::GET, &base, None).await?;
        let missing: Vec<String> = roles
            .iter()
            .filter(|r| !names(&current).contains(r))
            .cloned()
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let body = Value::Array(Self::pick_roles(&available, &missing)?);
        self.ok(Method::POST, &base, Some(&body)).await.map(|_| ())
    }

    async fn remove_realm_roles(&self, id: &str, roles: &[String]) -> Result<(), KernelError> {
        if roles.is_empty() {
            return Ok(());
        }
        let base = format!("/users/{}/role-mappings/realm", enc(id));
        let current = self.ok(Method::GET, &base, None).await?;
        let present: Vec<String> = roles
            .iter()
            .filter(|r| names(&current).contains(r))
            .cloned()
            .collect();
        if present.is_empty() {
            return Ok(());
        }
        let body = Value::Array(Self::pick_roles(&current, &present)?);
        self.ok(Method::DELETE, &base, Some(&body))
            .await
            .map(|_| ())
    }

    async fn client_roles(&self, id: &str, client_id: &str) -> Result<Vec<String>, KernelError> {
        let client = self.client_uuid(client_id).await?;
        let v = self
            .ok(
                Method::GET,
                &format!("/users/{}/role-mappings/clients/{}", enc(id), enc(&client)),
                None,
            )
            .await?;
        Ok(names(&v))
    }

    async fn add_client_roles(
        &self,
        id: &str,
        client_id: &str,
        roles: &[String],
    ) -> Result<(), KernelError> {
        if roles.is_empty() {
            return Ok(());
        }
        let client = self.client_uuid(client_id).await?;
        let base = format!("/users/{}/role-mappings/clients/{}", enc(id), enc(&client));
        let current = self.ok(Method::GET, &base, None).await?;
        let missing: Vec<String> = roles
            .iter()
            .filter(|r| !names(&current).contains(r))
            .cloned()
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let available = self
            .ok(Method::GET, &format!("{base}/available"), None)
            .await?;
        let body = Value::Array(Self::pick_roles(&available, &missing)?);
        self.ok(Method::POST, &base, Some(&body)).await.map(|_| ())
    }

    async fn remove_client_roles(
        &self,
        id: &str,
        client_id: &str,
        roles: &[String],
    ) -> Result<(), KernelError> {
        if roles.is_empty() {
            return Ok(());
        }
        let client = self.client_uuid(client_id).await?;
        let base = format!("/users/{}/role-mappings/clients/{}", enc(id), enc(&client));
        let current = self.ok(Method::GET, &base, None).await?;
        let present: Vec<String> = roles
            .iter()
            .filter(|r| names(&current).contains(r))
            .cloned()
            .collect();
        if present.is_empty() {
            return Ok(());
        }
        let body = Value::Array(Self::pick_roles(&current, &present)?);
        self.ok(Method::DELETE, &base, Some(&body))
            .await
            .map(|_| ())
    }

    async fn groups(&self, id: &str) -> Result<Vec<IdpGroup>, KernelError> {
        let v = self
            .ok(
                Method::GET,
                &format!(
                    "/users/{}/groups?briefRepresentation=true&max=1000",
                    enc(id)
                ),
                None,
            )
            .await?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|g| {
                        Some(IdpGroup {
                            id: g.get("id")?.as_str()?.to_owned(),
                            path: g.get("path")?.as_str()?.to_owned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn group_by_path(&self, path: &str) -> Result<Option<String>, KernelError> {
        let encoded: Vec<String> = path
            .trim_start_matches('/')
            .split('/')
            .map(|seg| enc(seg).replace('+', "%20"))
            .collect();
        let (status, _, v) = self
            .call(
                Method::GET,
                &format!("/group-by-path/{}", encoded.join("/")),
                None,
            )
            .await?;
        match status {
            s if s.is_success() => Ok(v.get("id").and_then(Value::as_str).map(str::to_owned)),
            StatusCode::NOT_FOUND => Ok(None),
            s => Err(status_error(s, "GET group-by-path", &v)),
        }
    }

    async fn ensure_scope_group(&self, group: &ScopeGroup) -> Result<String, KernelError> {
        let tenant_name = ScopeGroup::tenant_group_name(group.tenant_id);
        let tenant_path = format!("/{tenant_name}");
        let tenant_attrs = json!({ "tenant_id": [group.tenant_id.to_string()] });
        let tenant_id = match self.group_by_path(&tenant_path).await? {
            Some(id) => id,
            None => {
                let rep = json!({ "name": tenant_name, "attributes": tenant_attrs });
                let (status, headers, v) = self.call(Method::POST, "/groups", Some(&rep)).await?;
                if status.is_success() {
                    location_id(&headers)
                        .ok_or_else(|| KernelError::Unavailable("group id missing".into()))?
                } else if status == StatusCode::CONFLICT {
                    // Created concurrently.
                    self.group_by_path(&tenant_path).await?.ok_or_else(|| {
                        KernelError::Unavailable("tenant group vanished after a conflict".into())
                    })?
                } else {
                    return Err(status_error(status, "POST group", &v));
                }
            }
        };
        let Some(institution) = group.institution_id else {
            return Ok(tenant_id);
        };
        let name = ScopeGroup::institution_group_name(institution);
        if let Some(id) = self.group_by_path(&group.path()).await? {
            return Ok(id);
        }
        let rep = json!({
            "name": name,
            "attributes": {
                "tenant_id": [group.tenant_id.to_string()],
                "institution_id": [institution.to_string()],
            },
        });
        let (status, headers, v) = self
            .call(
                Method::POST,
                &format!("/groups/{}/children", enc(&tenant_id)),
                Some(&rep),
            )
            .await?;
        if status.is_success() {
            if let Some(id) = location_id(&headers) {
                return Ok(id);
            }
        } else if status != StatusCode::CONFLICT {
            return Err(status_error(status, "POST child group", &v));
        }
        match self.group_by_path(&group.path()).await? {
            Some(id) => Ok(id),
            None => self
                .find_group_child(&tenant_id, &name)
                .await?
                .ok_or_else(|| KernelError::Unavailable("institution group not found".into())),
        }
    }

    async fn join_group(&self, id: &str, group_id: &str) -> Result<(), KernelError> {
        self.ok(
            Method::PUT,
            &format!("/users/{}/groups/{}", enc(id), enc(group_id)),
            None,
        )
        .await
        .map(|_| ())
    }

    async fn leave_group(&self, id: &str, group_id: &str) -> Result<(), KernelError> {
        let (status, _, v) = self
            .call(
                Method::DELETE,
                &format!("/users/{}/groups/{}", enc(id), enc(group_id)),
                None,
            )
            .await?;
        if status.is_success() || status == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(status_error(status, "DELETE group membership", &v))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(claims: &Value) -> String {
        let e = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
        format!(
            "{}.{}.sig",
            e(br#"{"alg":"RS256"}"#),
            e(claims.to_string().as_bytes())
        )
    }

    #[test]
    fn least_privilege_admin_client_is_accepted() {
        let t = jwt(&json!({
            "exp": 1,
            "realm_access": {"roles": ["default-roles-sutra", "offline_access"]},
            "resource_access": {"realm-management": {"roles": [
                "manage-users", "view-users", "query-users", "query-groups", "query-clients"]}},
        }));
        let g = token_grants(&t).unwrap_or_default();
        assert_eq!(g.exp, Some(1));
        assert!(check_admin_grants(&g).is_ok());
    }

    #[test]
    fn over_and_under_privileged_admin_clients_are_refused() {
        let grants = |rm: &[&str], realm: &[&str]| AdminTokenGrants {
            exp: None,
            realm_management: rm.iter().map(|s| (*s).to_owned()).collect(),
            realm_roles: realm.iter().map(|s| (*s).to_owned()).collect(),
        };
        let base = ["manage-users", "view-users", "query-users", "query-groups"];
        for extra in [
            "realm-admin",
            "manage-realm",
            "manage-clients",
            "view-clients",
            "view-realm",
            "impersonation",
        ] {
            let mut rm = base.to_vec();
            rm.push(extra);
            let e = check_admin_grants(&grants(&rm, &[])).unwrap_err();
            assert!(e.contains("over-privileged") && e.contains(extra), "{e}");
        }
        let e = check_admin_grants(&grants(&base[..3], &[])).unwrap_err();
        assert!(e.contains("lacks") && e.contains("query-groups"), "{e}");
        let e = check_admin_grants(&grants(&base, &["administrator"])).unwrap_err();
        assert!(e.contains("SutraERP role"), "{e}");
        assert!(token_grants("not-a-jwt").is_err());
    }

    #[test]
    fn location_and_error_mapping() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::LOCATION,
            reqwest::header::HeaderValue::from_static("http://kc/admin/realms/sutra/users/1b2c-3d"),
        );
        assert_eq!(location_id(&h).as_deref(), Some("1b2c-3d"));
        assert!(matches!(
            status_error(StatusCode::CONFLICT, "x", &json!({"errorMessage": "User exists"})),
            KernelError::Conflict(m) if m.contains("User exists")
        ));
        assert!(matches!(
            status_error(StatusCode::BAD_GATEWAY, "x", &Value::Null),
            KernelError::Unavailable(_)
        ));
        assert!(matches!(
            status_error(StatusCode::FORBIDDEN, "x", &Value::Null),
            KernelError::Forbidden(_)
        ));
        assert_eq!(enc("a b/c"), "a+b%2Fc");
    }
}
