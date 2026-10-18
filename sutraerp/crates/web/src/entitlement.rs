//! Server-side module entitlement and licence gate (Part 2 · S7-entitle;
//! `docs/readiness/ENTITLEMENT_IMPLEMENTATION.md`).
//!
//! [`enforce`] is a global middleware that runs **inside** [`crate::auth::authenticate`] (it needs
//! the resolved [`AuthContext`], and its store lookup runs under the session's row-level security
//! scope). For every authenticated request it resolves the tenant's effective entitlement through
//! the [`EntitlementPolicy`] and applies [`sutra_kernel::entitlement::decide`]:
//!
//! * a path of a module the tenant has not activated → `403 entitlement.module_inactive`, whatever
//!   the caller's roles (the UI hiding a module is never the control);
//! * licence expired → reads and data export still work, writes → `403 entitlement.licence_expired`;
//! * licence suspended → only the platform administration area (holders of
//!   `platform.entitlement.manage`) and data export → otherwise `403 entitlement.licence_suspended`;
//! * licence in grace → full access; every response carries `Sutra-Licence-Status: grace` so the
//!   client can show its renewal banner (also sent for `expired` / `suspended`).
//!
//! Unauthenticated requests pass untouched (the handlers answer 401). Sign-in / sign-out and the
//! entitlement endpoints are never gated. A store or registry failure fails **closed** (503).

use crate::auth::is_unsafe;
use crate::problem::ApiError;
use crate::Problem;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::entitlement::{self, Access, EntitlementPolicy, LicenceStatus, RequestFacts};

/// Response header naming a non-active licence status (`grace`, `expired`, `suspended`).
pub const LICENCE_STATUS_HEADER: &str = "sutra-licence-status";

/// State of the [`enforce`] middleware.
#[derive(Clone)]
pub struct EntitlementGate {
    policy: EntitlementPolicy,
}

impl EntitlementGate {
    pub fn new(policy: EntitlementPolicy) -> Self {
        Self { policy }
    }

    pub fn policy(&self) -> &EntitlementPolicy {
        &self.policy
    }
}

/// Global entitlement / licence middleware (see the module docs).
pub async fn enforce(State(gate): State<EntitlementGate>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_owned();
    if entitlement::is_always_allowed(&path) {
        return next.run(req).await;
    }
    let Some((tenant, platform_admin)) = req.extensions().get::<AuthContext>().map(|ctx| {
        (
            ctx.scope.tenant_id,
            ctx.has_permission(Permission::EntitlementManage),
        )
    }) else {
        return next.run(req).await;
    };
    let reg = match entitlement::registry() {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = e, "entitlement registry invalid; refusing request");
            return Problem::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "entitlement.unavailable",
                "Entitlements unavailable",
            )
            .detail("the module entitlement registry could not be loaded")
            .into_response();
        }
    };
    let ent = match gate.policy.effective(tenant).await {
        Ok(e) => e,
        Err(e) => return ApiError(e).into_response(),
    };
    let facts = RequestFacts {
        path: &path,
        unsafe_method: is_unsafe(req.method()),
        platform_admin,
    };
    let status = ent.status;
    let mut res = match entitlement::decide(reg, &ent, &facts) {
        Access::Allow => next.run(req).await,
        Access::Deny {
            code,
            title,
            detail,
            module,
        } => {
            tracing::info!(
                error_code = code,
                module = module.as_deref().unwrap_or("core"),
                licence = status.as_str(),
                %path,
                "request refused by the entitlement gate"
            );
            Problem::new(StatusCode::FORBIDDEN, code, title)
                .detail(detail)
                .into_response()
        }
    };
    if status != LicenceStatus::Active {
        res.headers_mut().insert(
            LICENCE_STATUS_HEADER,
            HeaderValue::from_static(status.as_str()),
        );
    }
    res
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::auth::{authenticate, PreSessionCsrf, SessionAuth};
    use axum::body::Body;
    use axum::routing::{get, post};
    use axum::Router;
    use chrono::{Duration, Utc};
    use http_body_util::BodyExt;
    use std::sync::{Arc, Mutex};
    use sutra_kernel::auth::{MfaLevel, Role, SessionResolver};
    use sutra_kernel::entitlement::{
        DefaultEntitlement, EntitlementOrigin, EntitlementStore, ManualClock, TenantEntitlement,
    };
    use sutra_kernel::error::KernelError;
    use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
    use tower::ServiceExt;

    const CSRF: &str = "c5rf-0000000000000000000000000000000000000000000000000000000000";

    struct Fixed(AuthContext);

    #[async_trait::async_trait]
    impl SessionResolver for Fixed {
        async fn resolve(&self, token: &str) -> Result<Option<AuthContext>, KernelError> {
            Ok((token == "t").then(|| self.0.clone()))
        }
    }

    #[derive(Default)]
    struct Store(Mutex<Option<TenantEntitlement>>);

    #[async_trait::async_trait]
    impl EntitlementStore for Store {
        async fn load(&self, _: TenantId) -> Result<Option<TenantEntitlement>, KernelError> {
            Ok(self.0.lock().unwrap().clone())
        }
    }

    fn ctx(role: Role) -> AuthContext {
        AuthContext {
            session_id: uuid::Uuid::now_v7(),
            subject: "dev|a".into(),
            login_id: "a".into(),
            display_name: "A".into(),
            roles: vec![role],
            scope: ExecutionScope::institution(TenantId::new(), InstitutionId::new()),
            csrf_token: CSRF.into(),
            expires_at: Utc::now(),
            idle_expires_at: Utc::now(),
            mfa_level: MfaLevel::None,
            amr: vec![],
            step_up_enforced: false,
            privileged_mfa_enforced: false,
        }
    }

    fn app(role: Role, store: Arc<Store>, clock: Arc<ManualClock>) -> Router {
        let auth = SessionAuth::new(
            Arc::new(Fixed(ctx(role))),
            "sid",
            Vec::<String>::new(),
            PreSessionCsrf::new([1u8; 32], false, 600),
        );
        let gate = EntitlementGate::new(EntitlementPolicy::new(
            store,
            clock,
            DefaultEntitlement::Package("complete_erp".into()),
        ));
        Router::new()
            .route(
                "/api/v1/exams/exam-sessions",
                get(|| async { "r" }).post(|| async { "w" }),
            )
            .route("/api/v1/people/payroll-runs", get(|| async { "r" }))
            .route(
                "/api/v1/platform/imports/entities/students/export",
                get(|| async { "csv" }),
            )
            .route("/api/v1/platform/entitlement", get(|| async { "status" }))
            .route("/api/v1/auth/logout", post(|| async { "bye" }))
            .layer(axum::middleware::from_fn_with_state(gate, enforce))
            .layer(axum::middleware::from_fn_with_state(auth, authenticate))
    }

    async fn call(
        app: &Router,
        method: &str,
        path: &str,
        cookie: bool,
    ) -> (StatusCode, String, Option<String>) {
        let mut b = Request::builder().method(method).uri(path);
        if cookie {
            b = b.header("cookie", "sid=t").header("x-csrf-token", CSRF);
        }
        let res = app
            .clone()
            .oneshot(b.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let licence = res
            .headers()
            .get(LICENCE_STATUS_HEADER)
            .map(|v| v.to_str().unwrap().to_owned());
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap_or(
            serde_json::Value::String(String::from_utf8_lossy(&body).into()),
        );
        let code = body["errorCode"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| body.as_str().unwrap_or_default().to_owned());
        (status, code, licence)
    }

    fn stored(package: &str, status: LicenceStatus, until_days: i64) -> TenantEntitlement {
        let now = Utc::now();
        TenantEntitlement {
            package: Some(package.into()),
            add_on_modules: vec![],
            status,
            valid_from: now - Duration::days(30),
            valid_until: Some(now + Duration::days(until_days)),
            grace_days: 10,
            version: 1,
            origin: EntitlementOrigin::Stored,
        }
    }

    #[tokio::test]
    async fn default_is_complete_erp_and_anonymous_requests_pass() {
        let store = Arc::new(Store::default());
        let a = app(
            Role::Administrator,
            store,
            Arc::new(ManualClock::new(Utc::now())),
        );
        assert_eq!(
            call(&a, "GET", "/api/v1/people/payroll-runs", true).await.0,
            StatusCode::OK
        );
        let (s, body, _) = call(&a, "GET", "/api/v1/exams/exam-sessions", false).await;
        assert_eq!(
            (s, body.as_str()),
            (StatusCode::OK, "r"),
            "no session: the handler decides"
        );
    }

    #[tokio::test]
    async fn inactive_module_is_403_even_for_an_administrator() {
        let store = Arc::new(Store(Mutex::new(Some(stored(
            "core_academic",
            LicenceStatus::Active,
            100,
        )))));
        let a = app(
            Role::Administrator,
            store,
            Arc::new(ManualClock::new(Utc::now())),
        );
        let (s, code, _) = call(&a, "GET", "/api/v1/people/payroll-runs", true).await;
        assert_eq!(
            (s, code.as_str()),
            (StatusCode::FORBIDDEN, "entitlement.module_inactive")
        );
        assert_eq!(
            call(&a, "POST", "/api/v1/exams/exam-sessions", true)
                .await
                .0,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn grace_expiry_and_renewal_follow_the_clock() {
        let store = Arc::new(Store(Mutex::new(Some(stored(
            "core_academic",
            LicenceStatus::Active,
            1,
        )))));
        let clock = Arc::new(ManualClock::new(Utc::now()));
        let a = app(Role::Faculty, store.clone(), clock.clone());
        let (s, _, lic) = call(&a, "POST", "/api/v1/exams/exam-sessions", true).await;
        assert_eq!((s, lic), (StatusCode::OK, None));
        clock.advance(Duration::days(5)); // past validUntil, within 10 grace days
        let (s, _, lic) = call(&a, "POST", "/api/v1/exams/exam-sessions", true).await;
        assert_eq!((s, lic.as_deref()), (StatusCode::OK, Some("grace")));
        clock.advance(Duration::days(10)); // past the grace period
        let (s, code, lic) = call(&a, "POST", "/api/v1/exams/exam-sessions", true).await;
        assert_eq!(
            (s, code.as_str(), lic.as_deref()),
            (
                StatusCode::FORBIDDEN,
                "entitlement.licence_expired",
                Some("expired")
            )
        );
        assert_eq!(
            call(&a, "GET", "/api/v1/exams/exam-sessions", true).await.0,
            StatusCode::OK,
            "read-only"
        );
        assert_eq!(
            call(
                &a,
                "GET",
                "/api/v1/platform/imports/entities/students/export",
                true
            )
            .await
            .0,
            StatusCode::OK,
            "export"
        );
        assert_eq!(
            call(&a, "POST", "/api/v1/auth/logout", true).await.0,
            StatusCode::OK
        );
        // Renewal: a new validity window restores writes at once.
        *store.0.lock().unwrap() = Some(stored("core_academic", LicenceStatus::Active, 400));
        let (s, _, lic) = call(&a, "POST", "/api/v1/exams/exam-sessions", true).await;
        assert_eq!((s, lic), (StatusCode::OK, None));
    }

    #[tokio::test]
    async fn suspended_leaves_export_and_status_only() {
        let store = Arc::new(Store(Mutex::new(Some(stored(
            "complete_erp",
            LicenceStatus::Suspended,
            100,
        )))));
        let a = app(Role::Student, store, Arc::new(ManualClock::new(Utc::now())));
        let (s, code, lic) = call(&a, "GET", "/api/v1/exams/exam-sessions", true).await;
        assert_eq!(
            (s, code.as_str(), lic.as_deref()),
            (
                StatusCode::FORBIDDEN,
                "entitlement.licence_suspended",
                Some("suspended")
            )
        );
        assert_eq!(
            call(
                &a,
                "GET",
                "/api/v1/platform/imports/entities/students/export",
                true
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            call(&a, "GET", "/api/v1/platform/entitlement", true)
                .await
                .0,
            StatusCode::OK
        );
    }
}
