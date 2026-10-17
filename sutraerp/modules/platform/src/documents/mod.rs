//! Platform document service — **Part 2 · Stage 4.2 (S4-docs)**.
//!
//! Files (admission documents, documents on a student's record) are kept in S3-compatible object
//! storage (MinIO on the college's own server, or AWS S3) behind
//! [`sutra_contracts::platform::documents::ObjectStorePort`]; their metadata lives in
//! `platform.document_object` (forced row-level security, identity columns guarded by a trigger,
//! never physically deleted). [`vault::PgDocumentVault`] implements the [`DocumentVault`] port the
//! owning modules call after they authorised the caller.
//!
//! Records-management API (`platform.audit.verify`, administrators / records officers):
//!
//! | Endpoint | |
//! |----------|-|
//! | `GET  /api/v1/platform/documents/{id}` | metadata (owner, hash, retention, hold) |
//! | `POST /api/v1/platform/documents/{id}/legal-hold` | `{ hold, reason }` place / release |
//! | `POST /api/v1/platform/documents/{id}/delete` | `{ reason }` — refused (409) within retention or under hold |
//!
//! [`DocumentRetentionSweep`] deletes documents whose retention ended (never under legal hold)
//! and finishes the object removal of deleted documents.

pub mod validate;
pub mod vault;

use axum::extract::{Path, State};
use axum::Json;
use sqlx::PgConnection;
use std::sync::Arc;
use std::time::Duration;
use sutra_contracts::platform::documents::{
    DocumentActor, DocumentVault, ObjectStorePort, StoredDocument,
};
use sutra_kernel::audit::AuditOutcome;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::audit;
use sutra_persistence::jobs::SweepJob;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub use vault::{DisabledDocumentVault, PgDocumentVault, VaultSettings};

/// Handler state of the records-management routes.
#[derive(Clone)]
pub struct DocumentsState {
    pub vault: Arc<dyn DocumentVault>,
}

/// Mounted by the composition root next to `crate::router`.
pub fn router(vault: Arc<dyn DocumentVault>) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(get_document))
        .routes(routes!(legal_hold))
        .routes(routes!(delete_document))
        .with_state(DocumentsState { vault })
}

fn actor(ctx: &AuthContext) -> DocumentActor {
    DocumentActor {
        id: Some(ctx.subject.clone()),
        correlation_id: sutra_web::request_id::current_correlation_id(),
    }
}

/// Body of `…/legal-hold`.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegalHoldBody {
    /// `true` places a hold, `false` releases it.
    pub hold: bool,
    /// Why (court order, investigation, …); 1–500 characters.
    pub reason: String,
}

/// Body of `…/delete`.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteDocumentBody {
    /// Why the document is deleted; 1–500 characters.
    pub reason: String,
}

/// Metadata of one document of the session's tenant (records management).
#[utoipa::path(
    get,
    path = "/api/v1/platform/documents/{id}",
    operation_id = "platform_document_get",
    tag = "platform-documents",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = StoredDocument),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Missing `platform.audit.verify`", body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 503, description = "Document storage is not configured", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_document(
    State(st): State<DocumentsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<StoredDocument>> {
    let ctx = auth.require(Permission::AuditVerify)?;
    Ok(Json(st.vault.describe_any(&ctx.scope, id).await?))
}

/// Place or release a legal hold (a held document cannot be deleted, not even after its
/// retention ended). Audited.
#[utoipa::path(
    post,
    path = "/api/v1/platform/documents/{id}/legal-hold",
    operation_id = "platform_document_legal_hold",
    tag = "platform-documents",
    params(("id" = String, Path, format = Uuid)),
    request_body = LegalHoldBody,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = StoredDocument),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Already in the requested state", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn legal_hold(
    State(st): State<DocumentsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<LegalHoldBody>,
) -> ApiResult<Json<StoredDocument>> {
    let ctx = auth.require(Permission::AuditVerify)?;
    Ok(Json(
        st.vault
            .legal_hold(&ctx.scope, id, b.hold, &b.reason, &actor(ctx))
            .await?,
    ))
}

/// Delete a document whose retention ended and that is not on legal hold (else 409, audited
/// as rejected). The bytes are removed from object storage; the metadata row stays.
#[utoipa::path(
    post,
    path = "/api/v1/platform/documents/{id}/delete",
    operation_id = "platform_document_delete",
    tag = "platform-documents",
    params(("id" = String, Path, format = Uuid)),
    request_body = DeleteDocumentBody,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = StoredDocument),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Under retention or legal hold", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn delete_document(
    State(st): State<DocumentsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<DeleteDocumentBody>,
) -> ApiResult<Json<StoredDocument>> {
    let ctx = auth.require(Permission::AuditVerify)?;
    Ok(Json(
        st.vault
            .delete(&ctx.scope, id, &b.reason, &actor(ctx))
            .await?,
    ))
}

/// Retention sweeper: (1) marks documents whose retention ended (and that are not on legal
/// hold) deleted, audited as `platform.document.expire` by `system:retention-sweep`; (2) removes
/// the objects of deleted documents from storage and stamps `purged_at`. Runs in the system
/// database context (every tenant), at most `batch_limit` rows per step.
pub struct DocumentRetentionSweep {
    pub store: Arc<dyn ObjectStorePort>,
    pub interval: Duration,
    pub batch_limit: u32,
}

/// Audit actor of the sweeper.
pub const SWEEP_ACTOR: &str = "system:retention-sweep";

#[async_trait::async_trait]
impl SweepJob for DocumentRetentionSweep {
    fn name(&self) -> &'static str {
        "document-retention-sweep"
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    async fn run(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        use sqlx::Row;
        let limit = i64::from(self.batch_limit.max(1));
        let expired = sqlx::query(
            "UPDATE platform.document_object SET deleted_at = now(), deleted_by = $2, \
             delete_reason = 'retention period ended', version = version + 1 \
             WHERE id IN (SELECT id FROM platform.document_object WHERE deleted_at IS NULL \
             AND NOT legal_hold AND retention_until <= now() ORDER BY retention_until \
             LIMIT $1 FOR UPDATE SKIP LOCKED) \
             RETURNING id, tenant_id, institution_id, owner_type, owner_id, sha256",
        )
        .bind(limit)
        .bind(SWEEP_ACTOR)
        .fetch_all(&mut *conn)
        .await?;
        let sweeper = DocumentActor {
            id: Some(SWEEP_ACTOR.to_owned()),
            correlation_id: None,
        };
        for r in &expired {
            let id: Uuid = r.try_get("id")?;
            let tenant: Uuid = r.try_get("tenant_id")?;
            let institution: Option<Uuid> = r.try_get("institution_id")?;
            let owner_type: String = r.try_get("owner_type")?;
            let owner_id: Uuid = r.try_get("owner_id")?;
            let sha: String = r.try_get("sha256")?;
            let scope = match institution {
                Some(i) => ExecutionScope::institution(TenantId(tenant), InstitutionId(i)),
                None => ExecutionScope::tenant(TenantId(tenant)),
            };
            audit::insert(
                &mut *conn,
                &vault::audit_entry(
                    &scope,
                    id,
                    institution,
                    "platform.document.expire",
                    AuditOutcome::Succeeded,
                    &sweeper,
                    serde_json::json!({ "ownerType": owner_type, "ownerId": owner_id, "sha256": sha }),
                ),
            )
            .await?;
        }
        let pending = sqlx::query(
            "SELECT id, storage_key FROM platform.document_object \
             WHERE deleted_at IS NOT NULL AND purged_at IS NULL ORDER BY deleted_at \
             LIMIT $1 FOR UPDATE SKIP LOCKED",
        )
        .bind(limit)
        .fetch_all(&mut *conn)
        .await?;
        let mut purged = 0u64;
        for r in &pending {
            let id: Uuid = r.try_get("id")?;
            let key: String = r.try_get("storage_key")?;
            match self.store.delete(&key).await {
                Ok(()) => {
                    sqlx::query(
                        "UPDATE platform.document_object SET purged_at = now() WHERE id = $1",
                    )
                    .bind(id)
                    .execute(&mut *conn)
                    .await?;
                    purged += 1;
                }
                Err(e) => {
                    tracing::warn!(document = %id, error = %e, "document object purge failed; retried next run");
                }
            }
        }
        Ok(u64::try_from(expired.len()).unwrap_or(0) + purged)
    }
}
