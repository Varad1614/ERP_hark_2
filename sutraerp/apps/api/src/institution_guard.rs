//! Audit remediation A3 (SEC-5): institution-scoped reads of an institution that is not the
//! caller's answer `404`, not `200` with an empty list.
//!
//! Every module scopes its queries by the session tenant (and row-level security), so a foreign
//! or made-up institution id in `/api/v1/<module>/institutions/{id}/…` never returned another
//! tenant's rows — but list endpoints answered `200 []`, which differs from the detail endpoint's
//! `404` and hides typos. There is no shared per-module helper every list handler goes through,
//! so this route middleware applies the rule once for all of them:
//!
//! * only signed-in `GET`s whose **route template** has an `institutions/{param}` segment;
//! * it runs **after** the handler and only re-checks *successful* answers, so the handler's own
//!   permission check (`403`) and validation stay first — a caller without the permission learns
//!   nothing about which institutions exist, and no extra query runs for refused requests;
//! * existence is asked of Domain B's [`InstitutionDirectory`] under the caller's tenant scope
//!   (another tenant's institution is indistinguishable from a missing one); known institutions
//!   are cached briefly so a page of reads costs one lookup.

use axum::extract::{MatchedPath, Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sutra_contracts::institution::InstitutionDirectory;
use sutra_kernel::auth::AuthContext;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_web::ApiError;
use uuid::Uuid;

/// How long a confirmed (tenant, institution) pair is trusted without asking again.
const KNOWN_TTL: Duration = Duration::from_secs(300);
/// Bound of the cache (cleared when full; institutions per deployment are few).
const KNOWN_MAX: usize = 10_000;

/// State of [`institution_guard`].
#[derive(Clone)]
pub struct InstitutionGuard {
    directory: Arc<dyn InstitutionDirectory>,
    known: Arc<Mutex<HashMap<(Uuid, Uuid), Instant>>>,
}

impl InstitutionGuard {
    pub fn new(directory: Arc<dyn InstitutionDirectory>) -> Self {
        Self {
            directory,
            known: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn is_known(&self, key: (Uuid, Uuid)) -> bool {
        self.known
            .lock()
            .map(|m| m.get(&key).is_some_and(|t| t.elapsed() < KNOWN_TTL))
            .unwrap_or(false)
    }

    fn remember(&self, key: (Uuid, Uuid)) {
        if let Ok(mut m) = self.known.lock() {
            if m.len() >= KNOWN_MAX {
                m.clear();
            }
            m.insert(key, Instant::now());
        }
    }
}

/// The institution id of `path` when `template` has an `institutions/{param}` segment.
pub fn institution_in_path(template: &str, path: &str) -> Option<Uuid> {
    let t: Vec<&str> = template.split('/').collect();
    let p: Vec<&str> = path.split('/').collect();
    if t.len() != p.len() {
        return None;
    }
    t.windows(2).enumerate().find_map(|(i, w)| {
        (w[0] == "institutions" && w[1].starts_with('{') && w[1].ends_with('}'))
            .then(|| p.get(i + 1).and_then(|s| Uuid::parse_str(s).ok()))
            .flatten()
    })
}

/// Route middleware (see the module docs).
pub async fn institution_guard(
    State(guard): State<InstitutionGuard>,
    req: Request,
    next: Next,
) -> Response {
    if req.method() != Method::GET {
        return next.run(req).await;
    }
    let target = match (
        req.extensions().get::<AuthContext>(),
        req.extensions().get::<MatchedPath>(),
    ) {
        (Some(ctx), Some(route)) => institution_in_path(route.as_str(), req.uri().path())
            .map(|id| (ctx.scope.tenant_id, id)),
        _ => None,
    };
    let Some((tenant, id)) = target else {
        return next.run(req).await;
    };
    let res = next.run(req).await;
    if !res.status().is_success() || guard.is_known((tenant.as_uuid(), id)) {
        return res;
    }
    match guard
        .directory
        .exists(&ExecutionScope::tenant(tenant), InstitutionId(id))
        .await
    {
        Ok(true) => {
            guard.remember((tenant.as_uuid(), id));
            res
        }
        Ok(false) => ApiError(KernelError::NotFound {
            entity: "institution",
        })
        .into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_institution_segment_of_the_template() {
        let id = "0192f5a0-7c3e-7000-8000-0000000000aa";
        assert_eq!(
            institution_in_path(
                "/api/v1/academic/institutions/{id}/offerings",
                &format!("/api/v1/academic/institutions/{id}/offerings"),
            ),
            Uuid::parse_str(id).ok()
        );
        assert_eq!(
            institution_in_path(
                "/api/v1/student/institutions/{institution_id}/students",
                &format!("/api/v1/student/institutions/{id}/students"),
            ),
            Uuid::parse_str(id).ok()
        );
        // Not institution-scoped, a literal segment, or a non-UUID value: no check.
        assert_eq!(
            institution_in_path("/api/v1/students/{id}", &format!("/api/v1/students/{id}")),
            None
        );
        assert_eq!(
            institution_in_path(
                "/api/v1/institution/institutions",
                "/api/v1/institution/institutions"
            ),
            None
        );
        assert_eq!(
            institution_in_path(
                "/api/v1/academic/institutions/{id}/offerings",
                "/api/v1/academic/institutions/nope/offerings",
            ),
            None
        );
    }
}
