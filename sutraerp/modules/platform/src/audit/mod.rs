//! Audit chain verification API — **owned by slice 9A** (`docs/batch9-contracts.md` §4.4).
//!
//! | Endpoint | Permission |
//! |----------|------------|
//! | `GET  /api/v1/platform/audit/chain` (`platform_audit_chain_status`) | `platform.audit.verify` |
//! | `POST /api/v1/platform/audit/chain/verify` (`platform_audit_chain_verify`) | `platform.audit.verify` + CSRF |
//! | `GET  /api/v1/platform/audit/anchors` (`platform_audit_anchor_list`) | `platform.audit.verify` |
//!
//! Always the **session** tenant's chain (no cross-tenant parameter). `verify` answers 200 even
//! when the chain is broken (`ok: false`, `firstBrokenSeq`, `reason`), is itself audited
//! (`platform.audit.chain.verify`) and emits a `sutra::metrics` event that feeds
//! `sutra_audit_chain_verify_failures_total` when it finds a break. Anchor MACs are checked with
//! the key the composition root installs as an [`axum::Extension`] of
//! [`sutra_persistence::audit_chain::AnchorKey`] (`SUTRA_AUDIT_ANCHOR_KEY`); without it anchors
//! are not checked.

use axum::extract::{Query, State};
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use sutra_kernel::audit::{ActorKind, AuditEntry, AuditOutcome};
use sutra_kernel::auth::Permission;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::secret::hex;
use sutra_persistence::audit_chain::{self, AnchorKey};
use sutra_persistence::{audit, map_db_error};
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Handler state.
#[derive(Clone)]
pub struct AuditState {
    pub pool: PgPool,
}

/// Mounted by `crate::router` (foundation mount point).
pub fn router(pool: PgPool) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(chain_status))
        .routes(routes!(chain_verify))
        .routes(routes!(anchor_list))
        .with_state(AuditState { pool })
}

/// Last anchor of a chain.
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnchorRef {
    pub seq: i64,
    pub key_id: String,
    pub anchored_at: DateTime<Utc>,
}

/// Head and counters of the session tenant's audit chain.
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChainStatusView {
    pub tenant_id: Uuid,
    /// `0` before the first seal.
    pub head_seq: i64,
    /// Hex SHA-256 of the head entry (`null` before the first seal).
    pub head_hash: Option<String>,
    /// Chain entries.
    pub sealed: i64,
    /// Audit rows not sealed yet (the sealer runs every `SUTRA_AUDIT_SEAL_SECS`).
    pub unsealed: i64,
    pub last_sealed_at: Option<DateTime<Utc>>,
    pub last_anchor: Option<AnchorRef>,
}

/// Range to verify (default: the whole chain).
#[derive(Clone, Debug, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VerifyRequest {
    /// First `seq` (≥ 1, default 1).
    pub from_seq: Option<i64>,
    /// Last `seq` (default: the head).
    pub to_seq: Option<i64>,
}

impl VerifyRequest {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        if self.from_seq.is_some_and(|s| s < 1) {
            v.push(FieldViolation::new(
                "fromSeq",
                "range",
                "fromSeq must be >= 1",
            ));
        }
        if self.to_seq.is_some_and(|s| s < 1) {
            v.push(FieldViolation::new("toSeq", "range", "toSeq must be >= 1"));
        }
        if let (Some(f), Some(t)) = (self.from_seq, self.to_seq) {
            if f > t {
                v.push(FieldViolation::new(
                    "toSeq",
                    "range",
                    "toSeq must not be before fromSeq",
                ));
            }
        }
        if v.is_empty() {
            Ok(())
        } else {
            Err(KernelError::Validation(v))
        }
    }
}

/// Verification outcome (200 even when the chain is broken).
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResult {
    pub ok: bool,
    /// Entries recomputed.
    pub checked: i64,
    pub first_broken_seq: Option<i64>,
    /// `hash_mismatch` | `link_broken` | `missing_row` | `anchor_mismatch`.
    pub reason: Option<String>,
    pub head_seq: i64,
    pub head_hash: Option<String>,
}

/// One anchor (HMAC-SHA-256 of the chain head).
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnchorView {
    pub seq: i64,
    pub head_hash: String,
    pub key_id: String,
    pub mac: String,
    pub anchored_at: DateTime<Utc>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct AnchorQuery {
    /// Newest first, 1–500 (default 50).
    pub limit: Option<u32>,
}

/// Head, sealed / unsealed counts and the last anchor of the session tenant's audit chain.
#[utoipa::path(
    get,
    path = "/api/v1/platform/audit/chain",
    operation_id = "platform_audit_chain_status",
    tag = "platform-audit",
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = ChainStatusView),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Missing `platform.audit.verify`", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn chain_status(
    State(st): State<AuditState>,
    auth: Authenticated,
) -> ApiResult<Json<ChainStatusView>> {
    let ctx = auth.require(Permission::AuditVerify)?;
    let tenant = ctx.scope.tenant_id.as_uuid();
    let s = audit_chain::status(&st.pool, tenant)
        .await
        .map_err(map_db_error)?;
    Ok(Json(ChainStatusView {
        tenant_id: tenant,
        head_seq: s.head_seq,
        head_hash: s.head_hash.map(|h| hex(&h)),
        sealed: s.sealed,
        unsealed: s.unsealed,
        last_sealed_at: s.last_sealed_at,
        last_anchor: s.last_anchor.map(|a| AnchorRef {
            seq: a.seq,
            key_id: a.key_id,
            anchored_at: a.anchored_at,
        }),
    }))
}

/// Recomputes the session tenant's chain (or a `seq` range) from the audit log and reports the
/// first broken entry. The verification itself is audited.
#[utoipa::path(
    post,
    path = "/api/v1/platform/audit/chain/verify",
    operation_id = "platform_audit_chain_verify",
    tag = "platform-audit",
    request_body = VerifyRequest,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, description = "Verification ran (`ok` false when the chain is broken)", body = VerifyResult),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Missing `platform.audit.verify` or CSRF token", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn chain_verify(
    State(st): State<AuditState>,
    auth: Authenticated,
    key: Option<Extension<AnchorKey>>,
    body: Option<Json<VerifyRequest>>,
) -> ApiResult<Json<VerifyResult>> {
    let ctx = auth.require(Permission::AuditVerify)?;
    let req = body.map(|Json(b)| b).unwrap_or_default();
    req.validate()?;
    let tenant = ctx.scope.tenant_id.as_uuid();
    let key = key.map(|Extension(k)| k);
    let r = audit_chain::verify(&st.pool, tenant, req.from_seq, req.to_seq, key.as_ref())
        .await
        .map_err(map_db_error)?;
    let reason = r.reason.map(|x| x.as_str().to_owned());
    if !r.ok {
        tracing::warn!(
            target: "sutra::metrics",
            audit_chain_verify_failed = true,
            first_broken_seq = r.first_broken_seq,
            reason = reason.as_deref().unwrap_or("-"),
            "audit chain verification failed"
        );
    }
    let outcome = if r.ok {
        AuditOutcome::Succeeded
    } else {
        AuditOutcome::Failed
    };
    audit::insert(
        &st.pool,
        &AuditEntry::new(ctx.scope, "platform.audit.chain.verify", outcome)
            .actor(ActorKind::User, Some(ctx.subject.clone()))
            .target("audit_chain", tenant.to_string())
            .correlation(sutra_web::request_id::current_correlation_id())
            .details(serde_json::json!({
                "fromSeq": req.from_seq,
                "toSeq": req.to_seq,
                "ok": r.ok,
                "checked": r.checked,
                "firstBrokenSeq": r.first_broken_seq,
                "reason": reason,
                "headSeq": r.head_seq,
                "anchorsChecked": key.is_some(),
            })),
    )
    .await
    .map_err(map_db_error)?;
    Ok(Json(VerifyResult {
        ok: r.ok,
        checked: i64::try_from(r.checked).unwrap_or(i64::MAX),
        first_broken_seq: r.first_broken_seq,
        reason,
        head_seq: r.head_seq,
        head_hash: r.head_hash.map(|h| hex(&h)),
    }))
}

/// Newest anchors of the session tenant's chain.
#[utoipa::path(
    get,
    path = "/api/v1/platform/audit/anchors",
    operation_id = "platform_audit_anchor_list",
    tag = "platform-audit",
    params(AnchorQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = Vec<AnchorView>),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Missing `platform.audit.verify`", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn anchor_list(
    State(st): State<AuditState>,
    auth: Authenticated,
    Query(q): Query<AnchorQuery>,
) -> ApiResult<Json<Vec<AnchorView>>> {
    let ctx = auth.require(Permission::AuditVerify)?;
    let limit = i64::from(q.limit.unwrap_or(50).clamp(1, 500));
    let rows = audit_chain::list_anchors(&st.pool, ctx.scope.tenant_id.as_uuid(), limit)
        .await
        .map_err(map_db_error)?;
    Ok(Json(
        rows.into_iter()
            .map(|a| AnchorView {
                seq: a.seq,
                head_hash: hex(&a.head_hash),
                key_id: a.key_id,
                mac: hex(&a.mac),
                anchored_at: a.anchored_at,
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_ranges_are_validated() {
        assert!(VerifyRequest::default().validate().is_ok());
        let ok = VerifyRequest {
            from_seq: Some(2),
            to_seq: Some(2),
        };
        assert!(ok.validate().is_ok());
        for bad in [
            VerifyRequest {
                from_seq: Some(0),
                to_seq: None,
            },
            VerifyRequest {
                from_seq: None,
                to_seq: Some(-1),
            },
            VerifyRequest {
                from_seq: Some(5),
                to_seq: Some(4),
            },
        ] {
            assert!(matches!(bad.validate(), Err(KernelError::Validation(_))));
        }
    }

    #[tokio::test]
    async fn router_registers_the_three_operations() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://x:y@127.0.0.1:1/z")
            .unwrap();
        let (_, api) = router(pool).split_for_parts();
        let ids: Vec<String> = api
            .paths
            .paths
            .values()
            .flat_map(|item| {
                [&item.get, &item.post]
                    .into_iter()
                    .flatten()
                    .filter_map(|op| op.operation_id.clone())
                    .collect::<Vec<_>>()
            })
            .collect();
        for id in [
            "platform_audit_chain_status",
            "platform_audit_chain_verify",
            "platform_audit_anchor_list",
        ] {
            assert!(ids.iter().any(|i| i == id), "{id} in {ids:?}");
        }
    }
}
