//! Document storage ports (Part 2 · Stage 4.2, S4-docs).
//!
//! * [`ObjectStorePort`] — S3-compatible object storage (MinIO on a college's own server, or AWS
//!   S3), realised by the composition root over `sutra_infra::s3`. Only the platform document
//!   service talks to it.
//! * [`DocumentVault`] — the platform document service every module uses to keep files: tenant-
//!   scoped object keys (`<tenant>/<institution>/<owner type>/<owner id>/<uuid>`), the
//!   `platform.document_object` metadata row (row-level security), file validation (size limit,
//!   allow-listed types by magic bytes, sanitised filename), SHA-256 integrity on upload and on
//!   every read, audit of upload / download / delete, retention and legal hold. Owning modules
//!   authorise the caller first (who may see the admission or student record) and then call the
//!   vault with the owner they resolved; the vault re-checks tenant, institution and owner, so a
//!   guessed document id or storage key never reaches a file of another record.

use chrono::{DateTime, Utc};
use std::time::Duration;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;
use uuid::Uuid;

/// Owner type of documents handed in for an admission (`student.admission_document`).
pub const OWNER_ADMISSION_DOCUMENT: &str = "admission_document";
/// Owner type of documents filed on a student's record (`student.student_document`).
pub const OWNER_STUDENT_DOCUMENT: &str = "student_document";

/// S3-compatible object storage.
#[async_trait::async_trait]
pub trait ObjectStorePort: Send + Sync + 'static {
    /// `s3` (log / metadata label).
    fn backend(&self) -> &'static str;
    /// The bucket answers and refuses anonymous access.
    async fn ready(&self) -> Result<(), KernelError>;
    /// Stores `body` (whose SHA-256 is `sha256`; the store verifies it on receipt).
    async fn put(
        &self,
        key: &str,
        content_type: &str,
        body: Vec<u8>,
        sha256: [u8; 32],
    ) -> Result<(), KernelError>;
    /// Reads `key`; an object larger than `max_bytes` is refused (`Conflict`), a missing one is
    /// `NotFound`.
    async fn get(&self, key: &str, max_bytes: u64) -> Result<Vec<u8>, KernelError>;
    /// Removes `key` (missing is fine).
    async fn delete(&self, key: &str) -> Result<(), KernelError>;
    /// Short-lived presigned GET (`attachment` disposition with `filename`).
    fn presign_get(
        &self,
        key: &str,
        ttl: Duration,
        filename: &str,
        content_type: &str,
    ) -> Result<String, KernelError>;
}

/// The record a document belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentOwner {
    pub owner_type: String,
    pub owner_id: Uuid,
}

impl DocumentOwner {
    pub fn new(owner_type: &str, owner_id: Uuid) -> Self {
        Self {
            owner_type: owner_type.to_owned(),
            owner_id,
        }
    }
}

/// Who acts (audit actor + correlation id).
#[derive(Clone, Debug, Default)]
pub struct DocumentActor {
    pub id: Option<String>,
    pub correlation_id: Option<String>,
}

/// A file to keep.
#[derive(Clone)]
pub struct NewDocument {
    pub owner: DocumentOwner,
    /// As uploaded (sanitised by the vault).
    pub filename: String,
    /// `Content-Type` the client declared (must agree with the detected type).
    pub declared_content_type: Option<String>,
    pub bytes: Vec<u8>,
    pub actor: DocumentActor,
}

impl std::fmt::Debug for NewDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewDocument")
            .field("owner", &self.owner)
            .field("filename", &self.filename)
            .field("bytes", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

/// Metadata of a stored document (`platform.document_object`). The storage key never leaves
/// the server.
#[derive(Clone, Debug, PartialEq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StoredDocument {
    pub id: Uuid,
    pub institution_id: Option<Uuid>,
    /// `admission_document` | `student_document`.
    pub owner_type: String,
    pub owner_id: Uuid,
    /// Sanitised file name.
    pub filename: String,
    /// `application/pdf` | `image/png` | `image/jpeg` (detected from the content).
    pub content_type: String,
    pub size_bytes: i64,
    /// Lower-case hex SHA-256 of the content, verified on every read.
    pub sha256: String,
    pub uploaded_by: Option<String>,
    pub uploaded_at: DateTime<Utc>,
    /// The document cannot be deleted before this instant.
    pub retention_until: DateTime<Utc>,
    pub legal_hold: bool,
    pub legal_hold_reason: Option<String>,
    /// Set when a read found the stored bytes changed or missing; downloads are refused.
    pub quarantined_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub version: i64,
}

/// Content plus metadata of a verified read.
#[derive(Clone)]
pub struct DocumentContent {
    pub document: StoredDocument,
    pub bytes: Vec<u8>,
}

/// A short-lived presigned download link (issued only after authorisation).
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentLink {
    pub url: String,
    pub expires_at: DateTime<Utc>,
}

/// Platform document service.
#[async_trait::async_trait]
pub trait DocumentVault: Send + Sync + 'static {
    /// `false` when no object storage is configured (every call answers 503).
    fn enabled(&self) -> bool;
    /// Largest accepted file (`SUTRA_DOCS_MAX_BYTES`).
    fn max_bytes(&self) -> u64;
    /// Validates, stores (object first, then metadata + audit in one transaction).
    async fn store(
        &self,
        scope: &ExecutionScope,
        doc: NewDocument,
    ) -> Result<StoredDocument, KernelError>;
    /// Metadata of a live document of one of `owners` (else `NotFound`).
    async fn describe(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
        owners: &[DocumentOwner],
    ) -> Result<StoredDocument, KernelError>;
    /// Reads, verifies size + SHA-256 (a mismatch quarantines the document and is audited) and
    /// audits the download.
    async fn fetch(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
        owners: &[DocumentOwner],
        actor: &DocumentActor,
    ) -> Result<DocumentContent, KernelError>;
    /// Re-reads and verifies the stored bytes without handing them out (verification step).
    async fn verify_integrity(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
        owners: &[DocumentOwner],
        actor: &DocumentActor,
    ) -> Result<StoredDocument, KernelError>;
    /// Presigned GET (only when `SUTRA_DOCS_PRESIGN_TTL_SECS` > 0); audited.
    async fn link(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
        owners: &[DocumentOwner],
        actor: &DocumentActor,
    ) -> Result<DocumentLink, KernelError>;
    /// Records-management read of any document of the scope (no owner check).
    async fn describe_any(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
    ) -> Result<StoredDocument, KernelError>;
    /// Places (`hold = true`) or releases a legal hold; audited.
    async fn legal_hold(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
        hold: bool,
        reason: &str,
        actor: &DocumentActor,
    ) -> Result<StoredDocument, KernelError>;
    /// Deletes a document whose retention has passed and that is not on hold (`Conflict`
    /// otherwise); the object is removed from storage; audited.
    async fn delete(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
        reason: &str,
        actor: &DocumentActor,
    ) -> Result<StoredDocument, KernelError>;
}
