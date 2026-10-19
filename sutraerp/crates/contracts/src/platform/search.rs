//! OpenSearch projection ports (Batch 9, slice 9C — `docs/batch9-contracts.md` §8).
//!
//! * [`SearchSource`] — read-only port implemented by each **owning** module over its own schema
//!   (`sutra_student::search_source`, `sutra_people::search_source`, `sutra_campus::search_source`).
//!   It turns an outbox event into the ids of the documents it touches and reads the current
//!   documents (incremental projection), and pages through every document (rebuild). The
//!   platform projector never runs SQL against a foreign schema.
//! * [`SearchIndex`] — the search engine (OpenSearch 2.x adapter in `sutra-infra`, wrapped by the
//!   composition root). Every document carries `tenantId` and every query filters on it.
//!
//! Documents are versioned with a monotonically increasing `version` derived from the owning
//! aggregate(s), written with external versioning (`external_gte`) so replays are idempotent and
//! an older replay never overwrites a newer document.

use sutra_kernel::error::KernelError;
use uuid::Uuid;

/// `students` (id, tenantId, institutionId, number, displayName, programme, status).
pub const PROJECTION_STUDENTS: &str = "students";
/// `staff` (id, tenantId, institutionId, number, displayName, department, status).
pub const PROJECTION_STAFF: &str = "staff";
/// `library-catalogue` (id, tenantId, institutionId, title, authors, isbn, subjects, availableCopies).
pub const PROJECTION_LIBRARY_CATALOGUE: &str = "library-catalogue";

/// Every v1 projection.
pub const PROJECTIONS: &[&str] = &[
    PROJECTION_STUDENTS,
    PROJECTION_STAFF,
    PROJECTION_LIBRARY_CATALOGUE,
];

/// Major version of every projection's mapping (`<prefix>-<projection>-v1`).
pub const MAPPING_MAJOR: u32 = 1;

/// One document as the owning module reads it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchDocument {
    /// `_id` = aggregate id.
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// Monotonic document version (external versioning).
    pub version: i64,
    /// The `_source` (camelCase members of the projection, always including `id` and `tenantId`).
    pub body: serde_json::Value,
}

/// One search hit.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub id: Uuid,
    pub projection: String,
    pub score: f64,
    /// The projected document (`_source`).
    #[schema(value_type = Object)]
    pub document: serde_json::Value,
}

/// Search response (`GET /api/v1/platform/search`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    pub projection: String,
    /// Total matching documents of the caller's tenant.
    pub total: u64,
    pub items: Vec<SearchHit>,
}

/// Read side of one projection, implemented by the module that owns the data.
#[async_trait::async_trait]
pub trait SearchSource: Send + Sync + 'static {
    /// One of [`PROJECTIONS`].
    fn projection(&self) -> &'static str;

    /// CloudEvents `type` prefixes whose events can change this projection (e.g.
    /// `sutraerp.student.student.`); the projector selects outbox rows with `LIKE '<prefix>%'`.
    fn event_prefixes(&self) -> &'static [&'static str];

    /// Whether an outbox event of this `type` can change this projection (pure).
    fn handles(&self, event_type: &str) -> bool {
        self.event_prefixes()
            .iter()
            .any(|p| event_type.starts_with(p))
    }

    /// Ids of the documents an event touches (may read the owning schema, e.g. loan → record).
    async fn affected(
        &self,
        tenant: Uuid,
        event_type: &str,
        subject: Option<&str>,
        data: &serde_json::Value,
    ) -> Result<Vec<Uuid>, KernelError>;

    /// Current documents of `ids` in `tenant` (ids that no longer exist are simply absent).
    async fn fetch(&self, tenant: Uuid, ids: &[Uuid]) -> Result<Vec<SearchDocument>, KernelError>;

    /// Every document of every tenant in id order after `after` (keyset paging, for rebuilds).
    async fn scan(
        &self,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<SearchDocument>, KernelError>;

    /// Number of documents (one tenant, or all tenants with `None`).
    async fn count(&self, tenant: Option<Uuid>) -> Result<u64, KernelError>;
}

/// The search engine behind the projections.
#[async_trait::async_trait]
pub trait SearchIndex: Send + Sync + 'static {
    /// Cluster reachable and at least yellow.
    async fn ready(&self) -> Result<(), KernelError>;

    /// Creates the index `<prefix>-<projection>-v1` (+ alias) when missing.
    async fn ensure(&self, projection: &str) -> Result<(), KernelError>;

    /// Drops and recreates the index of `projection` (rebuild).
    async fn recreate(&self, projection: &str) -> Result<(), KernelError>;

    /// `_bulk` index with external (`external_gte`) versions; returns the documents written.
    async fn upsert(&self, projection: &str, docs: &[SearchDocument]) -> Result<u64, KernelError>;

    /// `_bulk` delete of `ids` (missing documents are ignored).
    async fn delete(&self, projection: &str, ids: &[Uuid]) -> Result<u64, KernelError>;

    /// Makes recent writes visible to search (tests, rebuild verification).
    async fn refresh(&self, projection: &str) -> Result<(), KernelError>;

    /// Full-text query restricted to `tenant` (always filtered; `q` empty = match all).
    async fn search(
        &self,
        projection: &str,
        tenant: Uuid,
        q: &str,
        limit: u32,
    ) -> Result<SearchResults, KernelError>;

    /// Number of documents (one tenant, or the whole index with `None`).
    async fn count(&self, projection: &str, tenant: Option<Uuid>) -> Result<u64, KernelError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projections_are_unique_and_kebab_case() {
        let set: std::collections::HashSet<_> = PROJECTIONS.iter().collect();
        assert_eq!(set.len(), PROJECTIONS.len());
        assert!(PROJECTIONS
            .iter()
            .all(|p| p.chars().all(|c| c.is_ascii_lowercase() || c == '-')));
    }

    #[test]
    fn documents_serialise_camel_case() {
        let d = SearchDocument {
            id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            version: 3,
            body: serde_json::json!({}),
        };
        let v = serde_json::to_value(&d).unwrap_or_default();
        assert!(v.get("tenantId").is_some());
    }
}
