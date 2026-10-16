//! The platform document service over PostgreSQL metadata (`platform.document_object`, forced
//! row-level security) and an [`ObjectStorePort`] (S3 / MinIO), plus the disabled vault used
//! when no storage is configured (development without MinIO, the OpenAPI build).

use super::validate;
use chrono::Utc;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Row};
use std::sync::Arc;
use std::time::Duration;
use sutra_contracts::platform::documents::{
    DocumentActor, DocumentContent, DocumentLink, DocumentOwner, DocumentVault, NewDocument,
    ObjectStorePort, StoredDocument,
};
use sutra_kernel::audit::{ActorKind, AuditEntry, AuditOutcome};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_kernel::secret::{ct_eq, hex, sha256};
use sutra_persistence::{audit, map_db_error};
use uuid::Uuid;

/// Tunables (`SUTRA_DOCS_*`).
#[derive(Clone, Debug)]
pub struct VaultSettings {
    /// Largest accepted file in bytes (`SUTRA_DOCS_MAX_BYTES`).
    pub max_bytes: u64,
    /// Minimum retention from upload (`SUTRA_DOCS_RETENTION_DAYS`; 0 only outside production).
    pub retention_days: u32,
    /// Lifetime of presigned download links; `None` = links disabled
    /// (`SUTRA_DOCS_PRESIGN_TTL_SECS=0`).
    pub presign_ttl: Option<Duration>,
}

pub(crate) const COLUMNS: &str = "id, institution_id, owner_type, owner_id, filename, \
    content_type, size_bytes, sha256, uploaded_by, uploaded_at, retention_until, legal_hold, \
    legal_hold_reason, quarantined_at, deleted_at, version";

pub(crate) fn from_row(r: &PgRow) -> Result<StoredDocument, sqlx::Error> {
    Ok(StoredDocument {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        owner_type: r.try_get("owner_type")?,
        owner_id: r.try_get("owner_id")?,
        filename: r.try_get("filename")?,
        content_type: r.try_get("content_type")?,
        size_bytes: r.try_get("size_bytes")?,
        sha256: r.try_get("sha256")?,
        uploaded_by: r.try_get("uploaded_by")?,
        uploaded_at: r.try_get("uploaded_at")?,
        retention_until: r.try_get("retention_until")?,
        legal_hold: r.try_get("legal_hold")?,
        legal_hold_reason: r.try_get("legal_hold_reason")?,
        quarantined_at: r.try_get("quarantined_at")?,
        deleted_at: r.try_get("deleted_at")?,
        version: r.try_get("version")?,
    })
}

fn not_found() -> KernelError {
    KernelError::NotFound { entity: "document" }
}

fn quarantined(doc: &StoredDocument) -> KernelError {
    KernelError::Conflict(format!(
        "document {} is quarantined after a failed integrity check",
        doc.id
    ))
}

fn valid_owner_type(t: &str) -> bool {
    let b = t.as_bytes();
    (2..=48).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

fn owned(doc: &StoredDocument, owners: &[DocumentOwner]) -> bool {
    owners
        .iter()
        .any(|o| o.owner_type == doc.owner_type && o.owner_id == doc.owner_id)
}

fn required_reason(reason: &str) -> Result<String, KernelError> {
    let r = reason.trim();
    if r.is_empty() || r.chars().count() > 500 {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "reason",
            "required",
            "a reason of 1-500 characters is required",
        )]));
    }
    Ok(r.to_owned())
}

/// Audit scope of a document: the session tenant + the document's institution.
fn audit_scope(scope: &ExecutionScope, institution: Option<Uuid>) -> ExecutionScope {
    match institution {
        Some(i) => ExecutionScope::institution(scope.tenant_id, InstitutionId(i)),
        None => ExecutionScope::tenant(scope.tenant_id),
    }
}

/// One audit entry about a document.
pub(crate) fn audit_entry(
    scope: &ExecutionScope,
    doc_id: Uuid,
    institution: Option<Uuid>,
    action: &str,
    outcome: AuditOutcome,
    actor: &DocumentActor,
    details: serde_json::Value,
) -> AuditEntry {
    let kind = match actor.id.as_deref() {
        Some(a) if a.starts_with("system:") => ActorKind::System,
        Some(_) => ActorKind::User,
        None => ActorKind::System,
    };
    AuditEntry::new(audit_scope(scope, institution), action, outcome)
        .actor(kind, actor.id.clone())
        .target("document_object", doc_id.to_string())
        .correlation(actor.correlation_id.clone())
        .details(details)
}

fn doc_details(doc: &StoredDocument) -> serde_json::Value {
    serde_json::json!({
        "ownerType": doc.owner_type,
        "ownerId": doc.owner_id,
        "filename": doc.filename,
        "contentType": doc.content_type,
        "sizeBytes": doc.size_bytes,
        "sha256": doc.sha256,
    })
}

/// [`DocumentVault`] over PostgreSQL + object storage.
pub struct PgDocumentVault {
    pool: PgPool,
    store: Arc<dyn ObjectStorePort>,
    settings: VaultSettings,
}

impl PgDocumentVault {
    pub fn new(pool: PgPool, store: Arc<dyn ObjectStorePort>, settings: VaultSettings) -> Self {
        Self {
            pool,
            store,
            settings,
        }
    }

    pub fn settings(&self) -> &VaultSettings {
        &self.settings
    }

    /// Live (not deleted) document of the scope's tenant (and institution when pinned) with its
    /// storage key and whether its retention still runs, optionally locked.
    async fn load(
        &self,
        conn: &mut PgConnection,
        scope: &ExecutionScope,
        id: Uuid,
        lock: bool,
    ) -> Result<(StoredDocument, String, bool), KernelError> {
        let sql = format!(
            "SELECT {COLUMNS}, storage_key, (retention_until > now()) AS retained \
             FROM platform.document_object WHERE id = $1 AND tenant_id = $2 \
             AND ($3::uuid IS NULL OR institution_id IS NULL OR institution_id = $3) \
             AND deleted_at IS NULL{}",
            if lock { " FOR UPDATE" } else { "" }
        );
        let row = sqlx::query(&sql)
            .bind(id)
            .bind(scope.tenant_id.as_uuid())
            .bind(scope.institution_id.map(|i| i.as_uuid()))
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db_error)?
            .ok_or_else(not_found)?;
        let doc = from_row(&row).map_err(map_db_error)?;
        let key: String = row.try_get("storage_key").map_err(map_db_error)?;
        let retained: bool = row.try_get("retained").map_err(map_db_error)?;
        Ok((doc, key, retained))
    }

    async fn load_owned(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
        owners: &[DocumentOwner],
    ) -> Result<(StoredDocument, String), KernelError> {
        let mut conn = self.pool.acquire().await.map_err(map_db_error)?;
        let (doc, key, _) = self.load(&mut conn, scope, id, false).await?;
        // A document of another record is indistinguishable from a missing one.
        if !owned(&doc, owners) {
            return Err(not_found());
        }
        Ok((doc, key))
    }

    async fn record(&self, entry: AuditEntry) -> Result<(), KernelError> {
        audit::insert(&self.pool, &entry)
            .await
            .map_err(map_db_error)
    }

    /// Reads the object and checks size + SHA-256; a mismatch or a missing object quarantines the
    /// document (committed and audited even though the request fails).
    async fn read_verified(
        &self,
        scope: &ExecutionScope,
        doc: &StoredDocument,
        key: &str,
        actor: &DocumentActor,
    ) -> Result<Vec<u8>, KernelError> {
        let size = u64::try_from(doc.size_bytes).unwrap_or(0);
        let failure = match self.store.get(key, size).await {
            Ok(bytes) => {
                let actual = hex(&sha256(&bytes));
                let same_size = u64::try_from(bytes.len()).ok() == Some(size);
                if same_size && ct_eq(actual.as_bytes(), doc.sha256.as_bytes()) {
                    return Ok(bytes);
                }
                format!(
                    "sha256 mismatch: {} bytes with sha256 {actual} stored",
                    bytes.len()
                )
            }
            Err(KernelError::NotFound { .. }) => "the stored object is missing".to_owned(),
            Err(KernelError::Conflict(_)) => "the stored object is larger than recorded".to_owned(),
            Err(e) => return Err(e),
        };
        tracing::error!(document = %doc.id, failure = %failure, "document integrity check failed");
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        sqlx::query(
            "UPDATE platform.document_object SET quarantined_at = now(), version = version + 1 \
             WHERE id = $1 AND quarantined_at IS NULL",
        )
        .bind(doc.id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        audit::insert(
            &mut *tx,
            &audit_entry(
                scope,
                doc.id,
                doc.institution_id,
                "platform.document.integrity_failed",
                AuditOutcome::Failed,
                actor,
                serde_json::json!({ "expectedSha256": doc.sha256, "failure": failure }),
            ),
        )
        .await
        .map_err(map_db_error)?;
        tx.commit().await.map_err(map_db_error)?;
        Err(KernelError::Conflict(format!(
            "document {} failed its integrity check ({failure}); it is quarantined",
            doc.id
        )))
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_metadata(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
        doc: &NewDocument,
        checked: &validate::Checked,
        key: &str,
        digest: &str,
        size: i64,
    ) -> Result<StoredDocument, KernelError> {
        let retention = i32::try_from(self.settings.retention_days).unwrap_or(i32::MAX);
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        let sql = format!(
            "INSERT INTO platform.document_object (id, tenant_id, institution_id, owner_type, \
             owner_id, filename, content_type, size_bytes, sha256, storage_key, storage_backend, \
             uploaded_by, retention_until) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, \
             $12, now() + make_interval(days => $13)) RETURNING {COLUMNS}"
        );
        let row = sqlx::query(&sql)
            .bind(id)
            .bind(scope.tenant_id.as_uuid())
            .bind(scope.institution_id.map(|i| i.as_uuid()))
            .bind(&doc.owner.owner_type)
            .bind(doc.owner.owner_id)
            .bind(&checked.filename)
            .bind(checked.file_type.mime)
            .bind(size)
            .bind(digest)
            .bind(key)
            .bind(self.store.backend())
            .bind(doc.actor.id.as_deref())
            .bind(retention)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
        let stored = from_row(&row).map_err(map_db_error)?;
        audit::insert(
            &mut *tx,
            &audit_entry(
                scope,
                stored.id,
                stored.institution_id,
                "platform.document.upload",
                AuditOutcome::Succeeded,
                &doc.actor,
                doc_details(&stored),
            ),
        )
        .await
        .map_err(map_db_error)?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(stored)
    }
}

#[async_trait::async_trait]
impl DocumentVault for PgDocumentVault {
    fn enabled(&self) -> bool {
        true
    }

    fn max_bytes(&self) -> u64 {
        self.settings.max_bytes
    }

    async fn store(
        &self,
        scope: &ExecutionScope,
        mut doc: NewDocument,
    ) -> Result<StoredDocument, KernelError> {
        let checked = validate::check(
            &doc.bytes,
            &doc.filename,
            doc.declared_content_type.as_deref(),
            self.settings.max_bytes,
        )?;
        if !valid_owner_type(&doc.owner.owner_type) {
            return Err(KernelError::internal_msg("invalid document owner type"));
        }
        let id = Uuid::now_v7();
        let institution = scope
            .institution_id
            .map(|i| i.as_uuid().to_string())
            .unwrap_or_else(|| "tenant".to_owned());
        // Tenant-rooted key; no file name (no personal data) in object storage paths.
        let key = format!(
            "{}/{institution}/{}/{}/{id}",
            scope.tenant_id, doc.owner.owner_type, doc.owner.owner_id
        );
        let digest = sha256(&doc.bytes);
        let size = i64::try_from(doc.bytes.len()).map_err(KernelError::internal)?;
        let bytes = std::mem::take(&mut doc.bytes);
        self.store
            .put(&key, checked.file_type.mime, bytes, digest)
            .await?;
        let stored = self
            .insert_metadata(scope, id, &doc, &checked, &key, &hex(&digest), size)
            .await;
        if stored.is_err() {
            // Never leave an object nobody can account for.
            if let Err(e) = self.store.delete(&key).await {
                tracing::warn!(error = %e, "could not remove the object of a failed upload");
            }
        }
        stored
    }

    async fn describe(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
        owners: &[DocumentOwner],
    ) -> Result<StoredDocument, KernelError> {
        Ok(self.load_owned(scope, id, owners).await?.0)
    }

    async fn fetch(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
        owners: &[DocumentOwner],
        actor: &DocumentActor,
    ) -> Result<DocumentContent, KernelError> {
        let (doc, key) = self.load_owned(scope, id, owners).await?;
        if doc.quarantined_at.is_some() {
            return Err(quarantined(&doc));
        }
        let bytes = self.read_verified(scope, &doc, &key, actor).await?;
        self.record(audit_entry(
            scope,
            doc.id,
            doc.institution_id,
            "platform.document.download",
            AuditOutcome::Succeeded,
            actor,
            doc_details(&doc),
        ))
        .await?;
        Ok(DocumentContent {
            document: doc,
            bytes,
        })
    }

    async fn verify_integrity(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
        owners: &[DocumentOwner],
        actor: &DocumentActor,
    ) -> Result<StoredDocument, KernelError> {
        let (doc, key) = self.load_owned(scope, id, owners).await?;
        if doc.quarantined_at.is_some() {
            return Err(quarantined(&doc));
        }
        self.read_verified(scope, &doc, &key, actor).await?;
        self.record(audit_entry(
            scope,
            doc.id,
            doc.institution_id,
            "platform.document.verify_integrity",
            AuditOutcome::Succeeded,
            actor,
            doc_details(&doc),
        ))
        .await?;
        Ok(doc)
    }

    async fn link(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
        owners: &[DocumentOwner],
        actor: &DocumentActor,
    ) -> Result<DocumentLink, KernelError> {
        let Some(ttl) = self.settings.presign_ttl else {
            return Err(KernelError::Forbidden(
                "presigned download links are disabled (SUTRA_DOCS_PRESIGN_TTL_SECS=0)".into(),
            ));
        };
        let (doc, key) = self.load_owned(scope, id, owners).await?;
        if doc.quarantined_at.is_some() {
            return Err(quarantined(&doc));
        }
        let url = self
            .store
            .presign_get(&key, ttl, &doc.filename, &doc.content_type)?;
        let expires_at =
            Utc::now() + chrono::Duration::from_std(ttl).map_err(KernelError::internal)?;
        self.record(audit_entry(
            scope,
            doc.id,
            doc.institution_id,
            "platform.document.link",
            AuditOutcome::Succeeded,
            actor,
            serde_json::json!({ "expiresAt": expires_at, "ttlSecs": ttl.as_secs() }),
        ))
        .await?;
        Ok(DocumentLink { url, expires_at })
    }

    async fn describe_any(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
    ) -> Result<StoredDocument, KernelError> {
        let mut conn = self.pool.acquire().await.map_err(map_db_error)?;
        Ok(self.load(&mut conn, scope, id, false).await?.0)
    }

    async fn legal_hold(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
        hold: bool,
        reason: &str,
        actor: &DocumentActor,
    ) -> Result<StoredDocument, KernelError> {
        let reason = required_reason(reason)?;
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        let (cur, _, _) = self.load(&mut tx, scope, id, true).await?;
        if cur.legal_hold == hold {
            return Err(KernelError::Conflict(format!(
                "the document is {} legal hold",
                if hold { "already on" } else { "not on" }
            )));
        }
        let sql = format!(
            "UPDATE platform.document_object SET legal_hold = $2, \
             legal_hold_reason = CASE WHEN $2 THEN $3 ELSE NULL END, legal_hold_by = $4, \
             legal_hold_at = now(), version = version + 1 WHERE id = $1 RETURNING {COLUMNS}"
        );
        let row = sqlx::query(&sql)
            .bind(id)
            .bind(hold)
            .bind(&reason)
            .bind(actor.id.as_deref())
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
        let doc = from_row(&row).map_err(map_db_error)?;
        audit::insert(
            &mut *tx,
            &audit_entry(
                scope,
                doc.id,
                doc.institution_id,
                if hold {
                    "platform.document.legal_hold.place"
                } else {
                    "platform.document.legal_hold.release"
                },
                AuditOutcome::Succeeded,
                actor,
                serde_json::json!({ "reason": reason }),
            ),
        )
        .await
        .map_err(map_db_error)?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(doc)
    }

    async fn delete(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
        reason: &str,
        actor: &DocumentActor,
    ) -> Result<StoredDocument, KernelError> {
        let reason = required_reason(reason)?;
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        let (cur, key, retained) = self.load(&mut tx, scope, id, true).await?;
        let refusal = if cur.legal_hold {
            Some("the document is under legal hold".to_owned())
        } else if retained {
            Some(format!(
                "the document is under retention until {}",
                cur.retention_until.to_rfc3339()
            ))
        } else {
            None
        };
        if let Some(why) = refusal {
            drop(tx);
            self.record(audit_entry(
                scope,
                cur.id,
                cur.institution_id,
                "platform.document.delete",
                AuditOutcome::Rejected,
                actor,
                serde_json::json!({ "reason": reason, "refused": why }),
            ))
            .await?;
            return Err(KernelError::Conflict(why));
        }
        let sql = format!(
            "UPDATE platform.document_object SET deleted_at = now(), deleted_by = $2, \
             delete_reason = $3, version = version + 1 WHERE id = $1 RETURNING {COLUMNS}"
        );
        let row = sqlx::query(&sql)
            .bind(id)
            .bind(actor.id.as_deref())
            .bind(&reason)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
        let doc = from_row(&row).map_err(map_db_error)?;
        audit::insert(
            &mut *tx,
            &audit_entry(
                scope,
                doc.id,
                doc.institution_id,
                "platform.document.delete",
                AuditOutcome::Succeeded,
                actor,
                serde_json::json!({ "reason": reason, "sha256": doc.sha256 }),
            ),
        )
        .await
        .map_err(map_db_error)?;
        tx.commit().await.map_err(map_db_error)?;
        // Remove the bytes; when storage is unavailable the retention sweeper finishes the purge.
        match self.store.delete(&key).await {
            Ok(()) => {
                sqlx::query(
                    "UPDATE platform.document_object SET purged_at = now() WHERE id = $1 \
                     AND purged_at IS NULL",
                )
                .bind(id)
                .execute(&self.pool)
                .await
                .map_err(map_db_error)?;
            }
            Err(e) => {
                tracing::warn!(document = %id, error = %e, "object removal deferred to the sweeper");
            }
        }
        Ok(doc)
    }
}

/// Vault without object storage (`SUTRA_DOCUMENT_STORAGE=disabled`, refused in production):
/// every call answers 503.
#[derive(Clone, Copy, Debug, Default)]
pub struct DisabledDocumentVault;

fn disabled() -> KernelError {
    KernelError::Unavailable("document storage is not configured".into())
}

#[async_trait::async_trait]
impl DocumentVault for DisabledDocumentVault {
    fn enabled(&self) -> bool {
        false
    }

    fn max_bytes(&self) -> u64 {
        0
    }

    async fn store(&self, _: &ExecutionScope, _: NewDocument) -> Result<StoredDocument, KernelError> {
        Err(disabled())
    }

    async fn describe(
        &self,
        _: &ExecutionScope,
        _: Uuid,
        _: &[DocumentOwner],
    ) -> Result<StoredDocument, KernelError> {
        Err(disabled())
    }

    async fn fetch(
        &self,
        _: &ExecutionScope,
        _: Uuid,
        _: &[DocumentOwner],
        _: &DocumentActor,
    ) -> Result<DocumentContent, KernelError> {
        Err(disabled())
    }

    async fn verify_integrity(
        &self,
        _: &ExecutionScope,
        _: Uuid,
        _: &[DocumentOwner],
        _: &DocumentActor,
    ) -> Result<StoredDocument, KernelError> {
        Err(disabled())
    }

    async fn link(
        &self,
        _: &ExecutionScope,
        _: Uuid,
        _: &[DocumentOwner],
        _: &DocumentActor,
    ) -> Result<DocumentLink, KernelError> {
        Err(disabled())
    }

    async fn describe_any(&self, _: &ExecutionScope, _: Uuid) -> Result<StoredDocument, KernelError> {
        Err(disabled())
    }

    async fn legal_hold(
        &self,
        _: &ExecutionScope,
        _: Uuid,
        _: bool,
        _: &str,
        _: &DocumentActor,
    ) -> Result<StoredDocument, KernelError> {
        Err(disabled())
    }

    async fn delete(
        &self,
        _: &ExecutionScope,
        _: Uuid,
        _: &str,
        _: &DocumentActor,
    ) -> Result<StoredDocument, KernelError> {
        Err(disabled())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_types_and_reasons() {
        assert!(valid_owner_type("admission_document"));
        assert!(!valid_owner_type("Admission"));
        assert!(!valid_owner_type("a/b"));
        assert!(!valid_owner_type("x"));
        assert!(required_reason("  ").is_err());
        assert!(required_reason(&"x".repeat(501)).is_err());
        assert_eq!(
            required_reason(" court order 12/2026 ").ok().as_deref(),
            Some("court order 12/2026")
        );
    }

    #[test]
    fn ownership_is_type_and_id() {
        let id = Uuid::now_v7();
        let doc = StoredDocument {
            id: Uuid::now_v7(),
            institution_id: None,
            owner_type: "admission_document".into(),
            owner_id: id,
            filename: "a.pdf".into(),
            content_type: "application/pdf".into(),
            size_bytes: 1,
            sha256: "0".repeat(64),
            uploaded_by: None,
            uploaded_at: Utc::now(),
            retention_until: Utc::now(),
            legal_hold: false,
            legal_hold_reason: None,
            quarantined_at: None,
            deleted_at: None,
            version: 1,
        };
        assert!(owned(&doc, &[DocumentOwner::new("admission_document", id)]));
        assert!(!owned(&doc, &[DocumentOwner::new("student_document", id)]));
        assert!(!owned(
            &doc,
            &[DocumentOwner::new("admission_document", Uuid::now_v7())]
        ));
        assert!(!owned(&doc, &[]));
    }

    #[tokio::test]
    async fn the_disabled_vault_answers_503() {
        let v = DisabledDocumentVault;
        assert!(!v.enabled());
        let s = ExecutionScope::tenant(sutra_kernel::scope::TenantId::new());
        assert!(matches!(
            v.describe_any(&s, Uuid::now_v7()).await,
            Err(KernelError::Unavailable(_))
        ));
    }
}
